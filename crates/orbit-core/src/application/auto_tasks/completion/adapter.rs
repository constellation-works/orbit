//! Run the granted completion adapter and read its verdict.

use orbit_automation::condition::CheckedAutoTaskStep;
use orbit_engine::activity_job::script_stdout_output;
use orbit_engine::{ScriptStepEnd, ScriptStepRequest, ScriptStepRun};
use orbit_types::task::Task;
use orbit_types::workflow::{
    AUTO_TASK_EVIDENCE_SCHEMA_VERSION, AutoTaskAdapterVerdict, AutoTaskCompletionContract,
    AutoTaskEvidenceCandidate,
};
use serde_json::{Map, Value, json};

use super::super::step_observation::StepContext;
use super::trust::{self, TrustedIdentity};
use crate::OrbitRuntime;
use crate::application::job::scripts::step::run_script_step;

pub(super) fn run_adapter(
    runtime: &OrbitRuntime,
    step: &StepContext,
    checked: &CheckedAutoTaskStep,
    contract: &AutoTaskCompletionContract,
    trusted: &TrustedIdentity,
    evidence: (&AutoTaskEvidenceCandidate, &Map<String, Value>),
    task: &Task,
) -> Result<(AutoTaskAdapterVerdict, String), String> {
    let (candidate, artifacts) = evidence;
    let unavailable = |why: String| {
        format!(
            "`{}` evidence is unavailable: adapter `{}` {why}",
            contract.contract, contract.adapter
        )
    };
    let tasks =
        trust::trusted_task_facts(runtime, contract, candidate, task).map_err(&unavailable)?;
    // Only release preparation needs pinned survey facts; obtaining them is
    // read-only Git and owner records, never candidate evidence.
    let release_survey = if contract.form == "release-prep" {
        Some(
            trust::trusted_release_survey(runtime, &trusted.binding.source_revision)
                .map_err(&unavailable)?,
        )
    } else {
        None
    };
    let input = json!({
        "schema_version": AUTO_TASK_EVIDENCE_SCHEMA_VERSION,
        "contract": contract.contract,
        "form": contract.form,
        "trusted": {
            "workspace_id": trusted.binding.workspace_id,
            "definition": trusted.binding.definition,
            "definition_digest": trusted.binding.definition_digest,
            "task_id": trusted.binding.task_id,
            "run_id": trusted.binding.run_id,
            "source_revision": trusted.binding.source_revision,
            "completed_at": trusted.completed_at,
            "execution_machine": trusted.execution_machine,
            "release_survey": release_survey,
        },
        "evidence": {"manifest": candidate, "artifacts": artifacts},
        "tasks": tasks,
    });
    let request = ScriptStepRequest {
        run_id: step.state.run_id.clone(),
        step_id: checked.id.clone(),
        script: contract.adapter.clone(),
        input,
        workspace_path: None,
        timeout_seconds: None,
    };
    let ran = match run_completion_adapter(runtime, &request) {
        ScriptStepRun::Ran(ran) => ran,
        ScriptStepRun::Refused { code, message, .. } => {
            return Err(unavailable(format!("was refused ({code}): {message}")));
        }
    };
    if ran.end != ScriptStepEnd::Exited(0) {
        return Err(unavailable(format!(
            "did not finish cleanly: {:?}",
            ran.end
        )));
    }
    let output = script_stdout_output(&ran.stdout, ran.stdout_over_cap, &ran.output_schema)
        .map_err(|error| unavailable(format!("gave no usable verdict: {error}")))?;
    let verdict: AutoTaskAdapterVerdict = serde_json::from_value(output)
        .map_err(|error| unavailable(format!("gave a malformed verdict: {error}")))?;
    if verdict.schema_version != AUTO_TASK_EVIDENCE_SCHEMA_VERSION
        || verdict.contract != contract.contract
        || verdict.form != contract.form
        || verdict.task_id != trusted.binding.task_id
        || verdict.source_revision != trusted.binding.source_revision
    {
        return Err(unavailable("answered about something else".to_string()));
    }
    Ok((verdict, ran.record.digest))
}

fn run_completion_adapter(runtime: &OrbitRuntime, request: &ScriptStepRequest) -> ScriptStepRun {
    #[cfg(feature = "test-support")]
    if let Some(ran) = super::test_support::completion_adapter_run(request) {
        return ran;
    }
    run_script_step(runtime, request)
}
