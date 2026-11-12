//! [ORB-24143] An implementer whose successful response fails only the output
//! contract (a bare `task_artifacts` file name) is asked once, in the same
//! implement step, for a corrected envelope. The engine repeats no side effect
//! and runs no retry or recovery; a response still malformed afterwards is
//! refused with the ordinary message.
//!
//! The provider is a scripted `grok` that numbers its invocations, appends a
//! line to a side-effect file only when it is not asked for a correction (as
//! an agent filing tasks would), and prints the scripted envelope.

use std::sync::Mutex;

use orbit_types::workflow::activity_job::V2AuditEventKind;
use serde_json::json;

use super::*;
use crate::git_fixture;

const MALFORMED_REFUSAL: &str =
    "must be an object with exactly string `path` and `source_path` fields";

struct CorrectionHost {
    cli: ScriptHost,
    delivered: Mutex<u32>,
    /// One entry per persisted invocation trace, in order: `true` when the
    /// dispatch's input carries `contract_correction`.
    traces: Mutex<Vec<bool>>,
}

impl orbit_engine::test_support::NullRuntimeHost for CorrectionHost {
    fn persist_invocation_trace(
        &self,
        _job_run_id: &str,
        _activity_id: &str,
        input: &Value,
        _invocation: &orbit_engine::DispatchInvocationTrace,
    ) -> Result<(), DispatchError> {
        self.traces
            .lock()
            .unwrap()
            .push(input.get("contract_correction").is_some());
        Ok(())
    }

    fn resolve_cli_executor(&self, provider: &str) -> Result<ResolvedCliExecutor, DispatchError> {
        RuntimeHost::resolve_cli_executor(&self.cli, provider)
    }

    fn run_deterministic(
        &self,
        action: &str,
        _: &Value,
        _: &Value,
        _: orbit_tools::ToolContext,
    ) -> Result<Value, DispatchError> {
        assert_eq!(action, "delivery_sentinel");
        *self.delivered.lock().unwrap() += 1;
        Ok(json!({}))
    }
}

struct Run {
    outcome: orbit_engine::activity_job::JobOutcome,
    events: Vec<orbit_types::workflow::activity_job::V2AuditEvent>,
    invocations: usize,
    side_effects: usize,
    correction_envelope: Option<Value>,
    delivered: u32,
    /// Persisted invocation traces in order, `true` for the correction's.
    traces: Vec<bool>,
}

/// A successful envelope whose `result` is `result`.
fn success_envelope(result: &Value) -> Value {
    json!({"schemaVersion":1,"status":"success","result":result,"error":null})
}

fn envelope_payload(envelope: &Value) -> String {
    json!({"text": envelope.to_string(), "stopReason": "stop"}).to_string()
}

/// Run `implement_one` then a delivery sentinel against a provider whose
/// successful results are `first` and `corrected`.
fn run_implementer(label: &str, first: &Value, corrected: &Value) -> Run {
    run_implementer_envelopes(
        label,
        &success_envelope(first),
        &success_envelope(corrected),
        0,
    )
}

/// Run `implement_one` then a delivery sentinel against a provider that
/// answers the full envelope `first` to the first invocation and `corrected`
/// to a correction, which exits with `corrected_exit`. A declared `failed`
/// envelope is protocol-valid only with a non-zero exit.
fn run_implementer_envelopes(
    label: &str,
    first: &Value,
    corrected: &Value,
    corrected_exit: u8,
) -> Run {
    let fixture = tempfile::tempdir().unwrap();
    let state = fixture.path().join("state");
    fs::create_dir(&state).unwrap();
    let script = format!(
        "#!/bin/sh\n\
         state='{state}'\n\
         n=$(cat \"$state/count\" 2>/dev/null || echo 0)\n\
         n=$((n+1))\n\
         echo \"$n\" > \"$state/count\"\n\
         cat > \"$state/prompt-$n\"\n\
         if grep -q 'contract_correction\":' \"$state/prompt-$n\"; then\n\
         cat <<'PAYLOAD'\n{corrected}\nPAYLOAD\n\
         exit {corrected_exit}\n\
         else\n\
         echo filed >> \"$state/side-effects\"\n\
         cat <<'PAYLOAD'\n{first}\nPAYLOAD\n\
         fi\n",
        state = state.display(),
        first = envelope_payload(first),
        corrected = envelope_payload(corrected),
        corrected_exit = corrected_exit,
    );
    let fake = fake_cli("grok", &script).unwrap();
    let host = CorrectionHost {
        cli: ScriptHost::new(fake.cli_path()),
        delivered: Mutex::new(0),
        traces: Mutex::new(Vec::new()),
    };
    let run_id = format!("contract-correction-{label}");
    let (writer, _) = build_writer(&fixture.path().join("audit"), &run_id).unwrap();
    let mut spec = cli_agent_loop_spec(Some(Provider::Grok));
    spec.model = None;
    let mut job = load_job_asset(
        &json!({
            "schemaVersion":2,"kind":"Job","metadata":{"name":"contract_correction_fixture"},
            "spec":{"state":"enabled","kind":"workflow","steps":[
                {"id":"implement_one","spec":ActivityV2Spec::AgentLoop(spec)},
                {"id":"git_commit","spec":{"type":"deterministic","action":"delivery_sentinel","config":{}}},
            ]},
        })
        .to_string(),
    )
    .unwrap()
    .spec;
    if let JobV2StepBody::Target(target) = &mut job.steps[0].body {
        target.activity_name = Some("agent_implement".into());
    }
    let outcome = execute_job_with_resume(
        &job,
        json!({"task_id":"fixture-task","workspace_path":fixture.path(),"claimed":false}),
        &run_id,
        writer.clone(),
        &host,
        None,
    )
    .unwrap();
    let invocations = fs::read_to_string(state.join("count"))
        .map(|count| count.trim().parse().unwrap())
        .unwrap_or(0);
    let side_effects = fs::read_to_string(state.join("side-effects"))
        .map(|lines| lines.lines().count())
        .unwrap_or(0);
    let correction_envelope = fs::read_to_string(state.join("prompt-2"))
        .ok()
        .map(|prompt| {
            let envelope = prompt.split_once("Execution envelope:\n").unwrap().1;
            serde_json::from_str(envelope.trim()).unwrap()
        });
    let delivered = *host.delivered.lock().unwrap();
    let traces = host.traces.lock().unwrap().clone();
    Run {
        events: writer.events_snapshot().unwrap(),
        outcome,
        invocations,
        side_effects,
        correction_envelope,
        delivered,
        traces,
    }
}

fn corrections(run: &Run) -> Vec<(&str, &str)> {
    run.events
        .iter()
        .filter_map(|event| match &event.kind {
            V2AuditEventKind::StepContractCorrection {
                step_id,
                field,
                outcome,
                ..
            } if step_id == "implement_one" => Some((field.as_str(), outcome.as_str())),
            _ => None,
        })
        .collect()
}

fn no_retry_or_recovery(run: &Run) -> bool {
    !run.events.iter().any(|event| {
        matches!(
            event.kind,
            V2AuditEventKind::StepRetry { .. }
                | V2AuditEventKind::StepRecoveryAttempted { .. }
                | V2AuditEventKind::StepPostRecoveryAttempt { .. }
                | V2AuditEventKind::FinalRecoveryAttempted { .. }
        )
    })
}

#[test]
fn a_malformed_task_artifacts_entry_is_corrected_within_the_implement_step() {
    git_fixture::isolated(
        module_path!(),
        "a_malformed_task_artifacts_entry_is_corrected_within_the_implement_step",
        || {
            let first = json!({
                "execution_summary": "Filed three tasks.",
                "task_artifacts": ["code-org-scan.json"],
            });
            // The correction restates only the corrected field.
            let corrected = json!({
                "task_artifacts": [{
                    "path": "code-org-scan.json",
                    "source_path": ".orbit/tmp/code-org-scan.json",
                }],
            });
            let run = run_implementer("corrected", &first, &corrected);

            assert!(run.outcome.success, "{:?}", run.outcome);
            assert_eq!(run.invocations, 2, "one implementation, one correction");
            assert_eq!(
                run.side_effects, 1,
                "the correction path files nothing a second time"
            );
            assert_eq!(run.delivered, 1, "delivery runs once, after the correction");
            assert_eq!(
                run.traces,
                [false, true],
                "the original and the corrected invocation are each persisted once"
            );
            assert_eq!(corrections(&run), [("task_artifacts", "requested")]);
            assert!(no_retry_or_recovery(&run), "{:?}", run.events);
            let started = run
                .events
                .iter()
                .filter(|event| {
                    matches!(&event.kind, V2AuditEventKind::StepStarted { step_id }
                        if step_id == "implement_one")
                })
                .count();
            assert_eq!(started, 1, "no second implement_one run");

            let output = &run.outcome.pipeline["implement_one"];
            assert_eq!(output["task_artifacts"], corrected["task_artifacts"]);
            assert_eq!(
                output["execution_summary"], "Filed three tasks.",
                "fields the correction does not restate are kept"
            );

            let correction = &run.correction_envelope.unwrap()["input"]["contract_correction"];
            assert_eq!(correction["field"], "task_artifacts");
            assert_eq!(correction["refused_value"], json!(["code-org-scan.json"]));
            assert!(
                correction["refusal"]
                    .as_str()
                    .unwrap()
                    .contains(MALFORMED_REFUSAL),
                "{correction}"
            );
        },
    );
}

#[test]
fn a_response_still_malformed_after_the_correction_is_refused_once() {
    git_fixture::isolated(
        module_path!(),
        "a_response_still_malformed_after_the_correction_is_refused_once",
        || {
            let first = json!({ "task_artifacts": ["code-org-scan.json"] });
            let still = json!({ "task_artifacts": ["scan.json"] });
            let run = run_implementer("still-malformed", &first, &still);

            assert!(!run.outcome.success, "{:?}", run.outcome);
            let message = run.outcome.message.as_deref().unwrap();
            assert!(
                message.contains(orbit_types::workflow::CONTRACT_REFUSAL_MARKER)
                    && message.contains("`task_artifacts[0]`")
                    && message.contains(MALFORMED_REFUSAL),
                "the existing refusal stands: {message}"
            );
            assert_eq!(run.invocations, 2, "the correction is asked once");
            assert_eq!(run.side_effects, 1);
            assert_eq!(run.delivered, 0, "a refused response delivers nothing");
            assert_eq!(
                run.traces,
                [false, true],
                "both invocations are persisted once"
            );
            assert_eq!(corrections(&run), [("task_artifacts", "requested")]);
        },
    );
}

#[test]
fn a_failed_correction_keeps_the_refusal_and_its_invocation_trace() {
    git_fixture::isolated(
        module_path!(),
        "a_failed_correction_keeps_the_refusal_and_its_invocation_trace",
        || {
            let first = json!({ "task_artifacts": ["code-org-scan.json"] });
            // The correction completes and declares failure instead of
            // returning a corrected envelope.
            let failed = json!({
                "schemaVersion": 1,
                "status": "failed",
                "result": {},
                "error": {"code": "correction_failed", "message": "could not restate the envelope"},
            });
            let run = run_implementer_envelopes(
                "correction-failed",
                &success_envelope(&first),
                &failed,
                1,
            );

            assert!(!run.outcome.success, "{:?}", run.outcome);
            let message = run.outcome.message.as_deref().unwrap();
            assert!(
                message.contains(orbit_types::workflow::CONTRACT_REFUSAL_MARKER)
                    && message.contains("`task_artifacts[0]`")
                    && message.contains(MALFORMED_REFUSAL),
                "the original refusal stands: {message}"
            );
            assert_eq!(run.invocations, 2, "the correction is asked once");
            assert_eq!(run.side_effects, 1, "the failed correction files nothing");
            assert_eq!(run.delivered, 0, "a refused response delivers nothing");
            assert_eq!(
                run.traces,
                [false, true],
                "the original and the failed correction are each persisted once"
            );
            assert_eq!(corrections(&run), [("task_artifacts", "requested")]);
        },
    );
}

#[test]
fn a_well_formed_response_is_never_corrected() {
    git_fixture::isolated(
        module_path!(),
        "a_well_formed_response_is_never_corrected",
        || {
            let first = json!({ "task_artifacts": [{
                "path": "scan.json", "source_path": ".orbit/tmp/scan.json",
            }] });
            let run = run_implementer("well-formed", &first, &first);

            assert!(run.outcome.success, "{:?}", run.outcome);
            assert_eq!(run.invocations, 1);
            assert!(corrections(&run).is_empty());
        },
    );
}
