//! Read compatibility for stored operator and explorer invocation results.
//!
//! New invocations are no longer supported. These projections keep old run
//! records observable through the ordinary run-show and log surfaces.

use orbit_common::text::floor_char_boundary;
use orbit_types::workflow::{JobRun, PipelineState};
use serde::Serialize;
use serde_json::{Map, Value};

use crate::runtime::audit::run::RunProviderProcess;

const AGENT_INVOKE_JOB_ID: &str = "agent_invoke_pipeline";
const EXPLORE_JOB_ID: &str = "agent_explore_pipeline";
const RESULT_PREVIEW_LIMIT_BYTES: usize = 4096;

/// A finished (or running) invocation, rendered for an operator.
///
/// Deliberately distinguishes the ways an invocation can end. A provider that
/// exits 0 without terminating its response envelope did not finish its turn,
/// and the run records that as a failure — so `outcome` here is never derived
/// from the exit code alone.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AgentInvokeResult {
    /// `running`, `queued`, `succeeded`, `failed`, `timeout`, `cancelled`,
    /// or `interrupted` — the run's own state, not the subprocess's exit code.
    pub outcome: String,
    /// Why a non-success outcome happened, when the run recorded a reason.
    pub failure_reason: Option<String>,
    /// The provider's exit code, when the subprocess ran to completion.
    pub exit_code: Option<i64>,
    /// Whether the subprocess was killed for exceeding its wall-clock bound.
    pub timed_out: bool,
    /// Whether the invocation terminated with a well-formed response envelope.
    /// `false` on an otherwise-clean exit means the agent stopped mid-turn.
    pub completed_envelope: bool,
    /// The agent's own summary, when it returned one. Also carried on
    /// `answer`; kept here for readers of the original shape.
    pub summary: Option<String>,
    /// What the agent answered [ORB-13899]. `None` until the invocation
    /// checkpoints a step output, which a failed one never does.
    pub answer: Option<AgentInvokeAnswer>,
    /// What the provider child is doing, or last did [ORB-13899]. `None` until
    /// its process is recorded.
    pub progress: Option<AgentInvokeProgress>,
    /// Bounded preview of the captured output.
    pub preview: Option<String>,
    /// Whether `preview` is shorter than what the invocation actually produced.
    pub preview_truncated: bool,
    /// Durable reference to the complete captured stdout, readable with
    /// `orbit run logs <RUN_ID>` after the preview is exhausted. Present for
    /// a failed invocation too, once its process has finished.
    pub stdout_blob_ref: Option<String>,
    /// Effective provider inner sandbox persisted at submission, as
    /// `provider:mode`. Absent on runs submitted before this field existed.
    pub provider_sandbox: Option<String>,
}

/// The agent's answer, as its response envelope and final message gave it
/// [ORB-13899].
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AgentInvokeAnswer {
    pub summary: Option<String>,
    /// The envelope's `findings`, in order. Usually strings; kept as JSON so a
    /// structured finding is not dropped.
    pub findings: Vec<Value>,
    pub next_steps: Vec<Value>,
    /// Every other field of the envelope `result` — `report_markdown`, for
    /// instance — exactly as the agent returned it.
    pub extra: Map<String, Value>,
    /// The agent's final assistant message, redacted and bounded from its
    /// start.
    pub final_message: Option<String>,
    /// Whether `final_message` was cut; the rest is in `final_message_blob_ref`.
    pub final_message_truncated: bool,
    /// Size of the final message before it was bounded.
    pub final_message_bytes: Option<u64>,
    /// The complete captured stdout holding the full message, when it was cut.
    pub final_message_blob_ref: Option<String>,
    /// The paths an explorer read to answer [ORB-16814]. `None` for an
    /// operator's invocation, whose `extra` keeps any such field as returned.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paths_read: Option<Vec<String>>,
}

/// Live evidence from the provider child [ORB-13899].
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AgentInvokeProgress {
    /// When the child last produced output, as RFC 3339. `None` before its
    /// first sampled output.
    pub last_activity_at: Option<String>,
    /// The newest assistant message sampled while it ran, bounded.
    pub latest_message: Option<String>,
    pub latest_message_truncated: bool,
}

/// Step-output keys an invocation's answer reads by name; every other
/// envelope `result` key is `extra`.
const ANSWER_FIELDS: [&str; 3] = ["summary", "findings", "next_steps"];

/// The envelope `result` key an explorer lists the paths it read under.
const PATHS_READ_FIELD: &str = "paths_read";

/// Read a finished or in-flight invocation for an operator [ORB-11354].
///
/// Returns `None` for any run that is not an agent invocation, so the ordinary
/// run projections can call it unconditionally.
///
/// Reads the run record and its persisted state rather than the live process:
/// results survive the submitting client disconnecting, and stay readable long
/// after the subprocess is gone.
///
/// `process` is the provider child the run's audit trail recorded, when the
/// caller read it: it carries live progress and, for a failed invocation, the
/// only reference to its captured output.
pub fn agent_invoke_result(
    run: &JobRun,
    state: Option<&PipelineState>,
    process: Option<&RunProviderProcess>,
) -> Option<AgentInvokeResult> {
    let explore = run.job_id == EXPLORE_JOB_ID;
    if run.job_id != AGENT_INVOKE_JOB_ID && !explore {
        return None;
    }
    let output = state
        .and_then(|state| state.step_output_entries().next_back())
        .map(|(_, output)| output);
    let field = |key: &str| output.and_then(|value| value.get(key));
    let stdout_blob_ref = field("stdout_blob_ref")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .or_else(|| process.and_then(|process| process.stdout_blob_ref.clone()));

    let timed_out = field("timed_out").and_then(Value::as_bool).unwrap_or(false);
    let preview = field("stdout_text")
        .and_then(Value::as_str)
        .map(bounded_preview);
    let preview_truncated = field("stdout_text_truncated")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || field("stdout_text")
            .and_then(Value::as_str)
            .is_some_and(|text| text.len() > RESULT_PREVIEW_LIMIT_BYTES);

    Some(AgentInvokeResult {
        outcome: outcome_label(run),
        failure_reason: run
            .steps
            .last()
            .and_then(|step| step.error_message.clone())
            .or_else(|| {
                field("completion_envelope_error")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
            }),
        exit_code: field("exit_code").and_then(Value::as_i64),
        timed_out,
        // Absent output means the invocation never reported, which is not the
        // same as reporting a complete envelope. Default to `false`.
        completed_envelope: field("completion_envelope_satisfied")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        summary: field("summary")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        answer: output.map(|output| invocation_answer(output, stdout_blob_ref.as_deref(), explore)),
        progress: process.map(|process| AgentInvokeProgress {
            last_activity_at: process.last_activity_at.map(|ts| ts.to_rfc3339()),
            latest_message: process.latest_message.clone(),
            latest_message_truncated: process.latest_message_truncated,
        }),
        preview,
        preview_truncated,
        stdout_blob_ref,
        provider_sandbox: run
            .input
            .as_ref()
            .and_then(|input| input.get("provider_sandbox"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
    })
}

/// The answer half of a checkpointed step output.
///
/// `extra` is the envelope `result` keys the engine recorded, less the named
/// ones. Outputs written before that record existed have no `extra`: Orbit's
/// own fields share the object and cannot be told apart from the agent's.
///
/// An explorer's answer also names the paths it read, lifted out of `extra`.
fn invocation_answer(
    output: &Value,
    stdout_blob_ref: Option<&str>,
    explore: bool,
) -> AgentInvokeAnswer {
    let field = |key: &str| output.get(key);
    let list = |key: &str| {
        field(key)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    let extra = field("response_result_fields")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|key| !ANSWER_FIELDS.contains(key))
        .filter(|key| !(explore && *key == PATHS_READ_FIELD))
        .filter_map(|key| field(key).map(|value| (key.to_string(), value.clone())))
        .collect();
    let final_message_truncated = field("final_message_truncated")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    AgentInvokeAnswer {
        summary: field("summary")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        findings: list("findings"),
        next_steps: list("next_steps"),
        extra,
        final_message: field("final_message")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        final_message_truncated,
        final_message_bytes: field("final_message_bytes").and_then(Value::as_u64),
        final_message_blob_ref: final_message_truncated
            .then(|| stdout_blob_ref.map(ToOwned::to_owned))
            .flatten(),
        paths_read: explore.then(|| {
            field(PATHS_READ_FIELD)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect()
        }),
    }
}

/// The run state, rendered as the outcome vocabulary an operator reads.
///
/// Taken from the run record, never from the provider's exit code: a
/// subprocess that exits 0 without finishing its turn produces a `failed` run,
/// and that is the answer to "did the investigation succeed".
fn outcome_label(run: &JobRun) -> String {
    run.state.to_string()
}

fn bounded_preview(text: &str) -> String {
    if text.len() <= RESULT_PREVIEW_LIMIT_BYTES {
        return text.to_string();
    }
    text[..floor_char_boundary(text, RESULT_PREVIEW_LIMIT_BYTES)].to_string()
}
