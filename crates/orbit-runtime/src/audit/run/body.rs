//! The reader-side view of one v2 audit envelope payload, parsed once where a
//! stored row is read.
//!
//! [`RunTrailBody`] is lenient on purpose: the producers write the strict
//! `orbit_types::workflow::V2AuditEventKind`, but durable rows can predate a
//! field or carry a malformed one. Every field a projection reads is optional
//! here and a value of the wrong JSON type reads as absent, so a historical
//! row projects exactly as it did when the readers walked the JSON by hand. A
//! projection that needs a field skips the row when it is absent.

use chrono::{DateTime, Utc};
use orbit_common::process::build_budget::BuildBudgetWaits;
use orbit_types::workflow::activity_job::ScriptExecutionRecord;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer};
use serde_json::Value;

/// The body of one v2 audit event, tagged by its payload `body_kind`.
///
/// Only the kinds the run-audit projections read have a variant. Every other
/// kind, and a payload whose `body_kind` is missing or not a string, is
/// [`Self::Other`].
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(tag = "body_kind", rename_all = "snake_case")]
pub enum RunTrailBody {
    StepStarted {
        #[serde(default, deserialize_with = "lenient")]
        step_id: Option<String>,
    },
    StepFinished {
        #[serde(default, deserialize_with = "lenient")]
        step_id: Option<String>,
        /// The recorded outcome text; the step projection reads an absent
        /// one as `finished`.
        #[serde(default, deserialize_with = "lenient")]
        outcome: Option<String>,
        #[serde(default, deserialize_with = "lenient")]
        error_message: Option<String>,
        /// Any non-null JSON value.
        #[serde(default)]
        error_diagnostic: Option<Value>,
        #[serde(default, deserialize_with = "lenient")]
        lock_wait_ms: Option<u64>,
    },
    StepSkipped {
        #[serde(default, deserialize_with = "lenient")]
        step_id: Option<String>,
        #[serde(default, deserialize_with = "lenient")]
        reason: Option<String>,
    },
    StepDenied {
        #[serde(default, deserialize_with = "lenient")]
        step_id: Option<String>,
        #[serde(default, deserialize_with = "lenient")]
        reason: Option<String>,
    },
    RunFinished {
        /// Any non-null JSON value.
        #[serde(default)]
        error_diagnostic: Option<Value>,
    },
    ActivityStarted {
        #[serde(default, deserialize_with = "lenient")]
        activity_name: Option<String>,
    },
    CliInvocationProcess {
        #[serde(default, deserialize_with = "lenient")]
        run_id: Option<String>,
        #[serde(default, deserialize_with = "lenient")]
        provider: Option<String>,
        /// Absent, or out of `u32` range, yields no provider process.
        #[serde(default, deserialize_with = "lenient")]
        pid: Option<u32>,
        #[serde(default, deserialize_with = "lenient")]
        pid_start_time: Option<String>,
    },
    CliInvocationActivity {
        #[serde(default, deserialize_with = "lenient")]
        latest_message: Option<String>,
        #[serde(default, deserialize_with = "lenient_flag")]
        latest_message_truncated: bool,
    },
    CliInvocationStoppedDescendant {
        /// Absent, or out of `u32` range, yields no descendant.
        #[serde(default, deserialize_with = "lenient")]
        pid: Option<u32>,
        #[serde(default, deserialize_with = "lenient")]
        pid_start_time: Option<String>,
        #[serde(default, deserialize_with = "lenient")]
        command: Option<String>,
        #[serde(default, deserialize_with = "lenient")]
        stopped_ms: Option<u64>,
        #[serde(default, deserialize_with = "lenient_flag")]
        ended: bool,
        #[serde(default, deserialize_with = "lenient")]
        error: Option<String>,
    },
    /// The cumulative waits, absent when any of them is missing or malformed.
    #[serde(deserialize_with = "lenient")]
    CliInvocationBuildBudget(Option<BuildBudgetWaits>),
    CliInvocationFinished {
        #[serde(default, deserialize_with = "lenient")]
        run_id: Option<String>,
        #[serde(default, deserialize_with = "lenient")]
        provider: Option<String>,
        #[serde(default, deserialize_with = "lenient")]
        exit_code: Option<i64>,
        #[serde(default, deserialize_with = "lenient_flag")]
        timed_out: bool,
        #[serde(default, deserialize_with = "lenient")]
        duration_ms: Option<u64>,
        #[serde(default, deserialize_with = "lenient")]
        stdout_blob_ref: Option<String>,
        #[serde(default, deserialize_with = "lenient")]
        stderr_blob_ref: Option<String>,
    },
    StepRecoveryAttempted {
        #[serde(default, deserialize_with = "lenient")]
        run_id: Option<String>,
        #[serde(default, deserialize_with = "lenient")]
        step_id: Option<String>,
        #[serde(default, deserialize_with = "lenient")]
        recovery_activity: Option<String>,
        #[serde(default, deserialize_with = "lenient")]
        recovery_succeeded: Option<bool>,
        #[serde(default, deserialize_with = "lenient")]
        failure_phase: Option<String>,
        #[serde(default, deserialize_with = "lenient")]
        error_message: Option<String>,
        #[serde(default, deserialize_with = "lenient")]
        decision: Option<RunTrailRecoveryDecision>,
    },
    FinalRecoveryAttempted {
        #[serde(default, deserialize_with = "lenient")]
        step_id: Option<String>,
        #[serde(default, deserialize_with = "lenient")]
        final_recovery_activity: Option<String>,
        #[serde(default, deserialize_with = "lenient")]
        outcome: Option<String>,
        /// The decision verdict acted on.
        #[serde(default, deserialize_with = "lenient")]
        decision: Option<String>,
        #[serde(default, deserialize_with = "lenient")]
        detail: Option<String>,
    },
    /// The attempt, absent when its `step_id` or record is malformed.
    #[serde(deserialize_with = "lenient")]
    ScriptExecution(Option<Box<RunTrailScriptExecution>>),
    /// Any other, missing or malformed `body_kind`.
    #[default]
    #[serde(other)]
    Other,
}

/// The read-back decision of a step recovery. Absent when `status` or
/// `retry_admitted` is missing or malformed.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct RunTrailRecoveryDecision {
    pub status: String,
    #[serde(default, deserialize_with = "lenient")]
    pub verdict: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    pub detail: Option<String>,
    pub retry_admitted: bool,
}

/// One script-step attempt as its `script_execution` body records it.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct RunTrailScriptExecution {
    pub step_id: String,
    #[serde(flatten)]
    pub record: ScriptExecutionRecord,
}

/// The envelope fields the projections read, each absent when missing or not
/// of its type.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub(super) struct RunTrailEnvelope {
    #[serde(default, deserialize_with = "lenient")]
    pub(super) event_id: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    pub(super) parent_event_id: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    pub(super) event_type: Option<String>,
    /// The kind tag as written, for a caller-supplied kind filter that may
    /// name a kind [`RunTrailBody`] keeps as [`RunTrailBody::Other`].
    #[serde(default, deserialize_with = "lenient")]
    pub(super) body_kind: Option<String>,
    /// The row's own `step_id`, not one inherited from an enclosing step.
    #[serde(default, deserialize_with = "lenient")]
    pub(super) step_id: Option<String>,
    /// An RFC 3339 `ts`; absent when missing or unparseable.
    #[serde(default, deserialize_with = "lenient_rfc3339")]
    pub(super) ts: Option<DateTime<Utc>>,
}

/// One stored envelope payload: the verbatim JSON plus its typed reading.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct RunTrailRow {
    pub(super) raw: Value,
    pub(super) envelope: RunTrailEnvelope,
    pub(super) body: RunTrailBody,
}

impl RunTrailRow {
    /// Parse a stored payload, or `None` when it is not JSON.
    pub(super) fn parse(payload_json: &str) -> Option<Self> {
        serde_json::from_str::<Value>(payload_json)
            .ok()
            .map(Self::from_value)
    }

    /// Read an already-decoded payload. A payload that is not a JSON object
    /// has no envelope fields and an [`RunTrailBody::Other`] body.
    pub(super) fn from_value(raw: Value) -> Self {
        let (envelope, body) = if raw.is_object() {
            (
                RunTrailEnvelope::deserialize(&raw).unwrap_or_default(),
                RunTrailBody::deserialize(&raw).unwrap_or_default(),
            )
        } else {
            (RunTrailEnvelope::default(), RunTrailBody::Other)
        };
        Self {
            raw,
            envelope,
            body,
        }
    }
}

/// A value of type `T`, or `None` when it is null or of another JSON type.
fn lenient<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
{
    let value = Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).ok())
}

/// A boolean that reads as `false` unless it is `true`.
fn lenient_flag<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(lenient::<D, bool>(deserializer)?.unwrap_or(false))
}

fn lenient_rfc3339<'de, D>(deserializer: D) -> Result<Option<DateTime<Utc>>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(lenient::<D, String>(deserializer)?
        .and_then(|value| DateTime::parse_from_rfc3339(&value).ok())
        .map(|value| value.with_timezone(&Utc)))
}
