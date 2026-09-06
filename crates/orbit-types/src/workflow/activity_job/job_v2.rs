// Existing expect calls in this module document local invariants; keep the allow scoped while the workspace lint is ratcheted.
#![allow(clippy::expect_used)]

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::workflow::JobScheduleState;

use super::activity_v2::{ActivityV2, ActivityV2Spec};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    #[default]
    Workflow,
    Subroutine,
}

impl std::fmt::Display for JobKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JobKind::Workflow => write!(f, "workflow"),
            JobKind::Subroutine => write!(f, "subroutine"),
        }
    }
}

/// v2 Job definition. Phase 3 adds first-class DAG constructs on each step.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JobV2 {
    pub state: JobScheduleState,
    #[serde(default)]
    pub default_input: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_activity: Option<String>,
    #[serde(skip)]
    pub resolved_recovery_activity: Option<ActivityV2>,
    /// Best-effort terminal hook invoked once after a step failure. Unlike
    /// `recovery_activity`, it does not retry or replace the failed step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_activity: Option<String>,
    #[serde(skip)]
    pub resolved_failure_activity: Option<ActivityV2>,
    #[serde(default = "default_max_active_runs")]
    pub max_active_runs: u32,
    #[serde(default)]
    pub kind: JobKind,
    pub steps: Vec<JobV2Step>,
}

/// A step in a v2 job. Carries `id`, optional `when` / `retry` modifiers,
/// and exactly one body (target / parallel / fan_out / loop).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct JobV2Step {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<RetrySpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_activity: Option<String>,
    #[serde(skip)]
    pub resolved_recovery_activity: Option<ActivityV2>,
    #[serde(flatten)]
    pub body: JobV2StepBody,
}

/// One-of body for a v2 step. Picked by exactly one of the `target`, `spec`,
/// `parallel`, `fan_out`, or `loop` body keys.
///
/// `TargetRef` and `Target` are distinct variants so the executor only ever
/// sees `Target` after [`super::catalog::resolve_job_target_refs`] runs at
/// load time. A `TargetRef` that survives into dispatch is a caller bug —
/// the job executor should never have to look up an activity by name.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(untagged)]
pub enum JobV2StepBody {
    Parallel {
        parallel: ParallelBlock,
    },
    FanOut {
        fan_out: FanOutBlock,
        fan_in: FanInSpec,
    },
    Loop {
        #[serde(rename = "loop")]
        loop_: LoopBlock,
    },
    TargetRef(TargetRef),
    Target(TargetStep),
}

impl<'de> Deserialize<'de> for JobV2Step {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        let mut v = Value::deserialize(deserializer)?;
        let obj = v
            .as_object_mut()
            .ok_or_else(|| D::Error::custom("step must be a mapping"))?;

        let id = obj
            .remove("id")
            .and_then(|x| x.as_str().map(String::from))
            .ok_or_else(|| D::Error::custom("step missing `id`"))?;
        let when = obj
            .remove("when")
            .and_then(|x| x.as_str().map(String::from));
        let retry = match obj.remove("retry") {
            Some(rv) => Some(
                serde_json::from_value::<RetrySpec>(rv)
                    .map_err(|e| D::Error::custom(format!("retry: {e}")))?,
            ),
            None => None,
        };
        let recovery_activity = obj
            .remove("recovery_activity")
            .and_then(|x| x.as_str().map(String::from));

        if obj.contains_key("role") {
            return Err(D::Error::custom(
                "step declares retired `role`; remove it and pass `crew` in the activity input to select a non-default crew",
            ));
        }

        let has_parallel = obj.contains_key("parallel");
        let has_fan_out = obj.contains_key("fan_out");
        let has_loop = obj.contains_key("loop");
        let has_target = obj.contains_key("target");
        let has_spec = obj.contains_key("spec");

        let body_shape_count = [has_parallel, has_fan_out, has_loop, has_target, has_spec]
            .iter()
            .filter(|present| **present)
            .count();
        if body_shape_count != 1 {
            return Err(D::Error::custom(
                "step must set exactly one body shape: `target`, `spec`, `parallel`, `fan_out`, or `loop`",
            ));
        }

        let body = match (has_parallel, has_fan_out, has_loop, has_target, has_spec) {
            (true, false, false, false, false) => {
                let block = obj
                    .remove("parallel")
                    .expect("parallel key was checked before step body dispatch");
                JobV2StepBody::Parallel {
                    parallel: serde_json::from_value(block)
                        .map_err(|e| D::Error::custom(format!("parallel: {e}")))?,
                }
            }
            (false, true, false, false, false) => {
                let fan_out_block = obj
                    .remove("fan_out")
                    .expect("fan_out key was checked before step body dispatch");
                let fan_in_block = obj
                    .remove("fan_in")
                    .ok_or_else(|| D::Error::custom("fan_out step missing matching `fan_in`"))?;
                JobV2StepBody::FanOut {
                    fan_out: serde_json::from_value(fan_out_block)
                        .map_err(|e| D::Error::custom(format!("fan_out: {e}")))?,
                    fan_in: serde_json::from_value(fan_in_block)
                        .map_err(|e| D::Error::custom(format!("fan_in: {e}")))?,
                }
            }
            (false, false, true, false, false) => {
                let block = obj
                    .remove("loop")
                    .expect("loop key was checked before step body dispatch");
                JobV2StepBody::Loop {
                    loop_: serde_json::from_value(block)
                        .map_err(|e| D::Error::custom(format!("loop: {e}")))?,
                }
            }
            (false, false, false, true, false) => JobV2StepBody::TargetRef(
                serde_json::from_value(Value::Object(std::mem::take(obj)))
                    .map_err(|e| D::Error::custom(format!("target ref: {e}")))?,
            ),
            (false, false, false, false, true) => JobV2StepBody::Target(
                serde_json::from_value(Value::Object(std::mem::take(obj)))
                    .map_err(|e| D::Error::custom(format!("target step: {e}")))?,
            ),
            _ => unreachable!("body shape count already validated"),
        };

        Ok(JobV2Step {
            id,
            when,
            retry,
            recovery_activity,
            resolved_recovery_activity: None,
            body,
        })
    }
}

/// Flat target step: inlines an `ActivityV2Spec` directly on the step. This
/// is the shape the executor operates on — [`TargetRef`] is rewritten to
/// `TargetStep` by [`super::catalog::resolve_job_target_refs`] before
/// dispatch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TargetStep {
    pub spec: ActivityV2Spec,
    /// Original catalog activity name when this target was produced from
    /// `target: activity:<name>`. Inline specs have no catalog name.
    #[serde(skip)]
    pub activity_name: Option<String>,
    /// Input contract from the resolved catalog activity. Inline specs have no
    /// catalog contract, so their template rendering remains strictly literal.
    #[serde(skip)]
    pub input_schema_json: Option<Value>,
    #[serde(rename = "fsProfile", default, skip_serializing_if = "Option::is_none")]
    pub fs_profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_input: Option<Value>,
    #[serde(default)]
    pub timeout_seconds: u64,
    /// Named Session binding. When set and present in the executor's session
    /// map (loop body only), conversation history persists across iterations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
}

/// Named target reference — `target: activity:<name>` in YAML. Phase 4
/// introduces this so job YAMLs can reference activities by name instead of
/// inlining the full spec. The resolver in
/// [`super::catalog::resolve_job_target_refs`] looks the name up in the
/// workspace catalog and rewrites this variant to [`TargetStep`] with the
/// named `ActivityV2Spec` inlined. All other fields (`default_input`,
/// `timeout_seconds`, `session`) carry through unchanged.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TargetRef {
    /// Reference string — currently `activity:<name>`. Namespace-prefixed so
    /// future kinds (`job:<name>` for nested-job refs, etc.) land without a
    /// breaking shape change.
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_input: Option<Value>,
    #[serde(default)]
    pub timeout_seconds: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
}

/// Retry modifier (§4.1). Applied per-step wrapper; counts re-runs of the
/// step body. Non-retryable errors (tool denial, unknown deterministic action,
/// validation) bypass retry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RetrySpec {
    pub max_attempts: u32,
    #[serde(default = "default_initial_backoff_ms")]
    pub initial_backoff_ms: u64,
    #[serde(default = "default_backoff_cap_ms")]
    pub backoff_cap_ms: u64,
    #[serde(default)]
    pub backoff_strategy: BackoffStrategy,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum BackoffStrategy {
    #[default]
    Exponential,
    Linear,
}

/// Parallel block (§4.2). Branches run concurrently; join policy decides the
/// block's success.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ParallelBlock {
    pub join: JoinMode,
    pub branches: Vec<JobV2Step>,
}

/// Explicit-form join selector (§12 Q4). YAML form:
/// ```yaml
/// join: { mode: all }
/// join: { mode: any }
/// join: { mode: quorum, n: 2 }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum JoinMode {
    All,
    Any,
    Quorum { n: u32 },
}

/// Fan-out block (§4.2). Dispatches a worker template against items with
/// bounded concurrency. Always paired with a `FanInSpec` on the same step.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FanOutBlock {
    /// Handlebars expression producing an iterable array (e.g.
    /// `{{ input.items }}` or `{{ steps.dispatch.output.tasks }}`).
    pub items: String,
    #[serde(default = "default_max_workers")]
    pub max_workers: u32,
    pub worker: Box<JobV2Step>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FanInSpec {
    pub join: JoinMode,
    /// Optional pipeline-context key to store the collected per-worker outputs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collect: Option<String>,
}

/// Loop block (§4.2). Bounded iteration with optional value-break and a shared
/// Session map across iterations.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LoopBlock {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub items: Option<String>,
    pub max_iterations: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub break_when: Option<String>,
    pub steps: Vec<JobV2Step>,
}

/// Structured pipeline-context reference (§5.2, §12 Q8).
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(untagged)]
pub enum PipelineRef {
    Block { from: String },
    Literal(String),
}

impl<'de> Deserialize<'de> for PipelineRef {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let v = Value::deserialize(deserializer)?;
        if let Some(s) = v.as_str() {
            return Ok(PipelineRef::Literal(s.to_string()));
        }
        if let Some(obj) = v.as_object()
            && let Some(from) = obj.get("from").and_then(|x| x.as_str())
        {
            return Ok(PipelineRef::Block {
                from: from.to_string(),
            });
        }
        Err(serde::de::Error::custom(
            "PipelineRef must be either a string or a block with `from:`",
        ))
    }
}

const fn default_max_active_runs() -> u32 {
    1
}

const fn default_initial_backoff_ms() -> u64 {
    1_000
}

const fn default_backoff_cap_ms() -> u64 {
    60_000
}

const fn default_max_workers() -> u32 {
    4
}
