use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::workflow::{JobScheduleState, ShipMode};

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
    /// This job creates a task-scoped worktree and may collect it after a
    /// successful delivery. The collector still checks task and Git safety.
    #[serde(default, skip_serializing_if = "is_false")]
    pub owns_task_worktree: bool,
    /// This job carries the tasks in its `input.task_ids` through delivery.
    /// A live run holds those tasks' delivery slot, so `orbit run ship`
    /// refuses to dispatch them again; `modes` says which ship modes a task
    /// may select the job for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_delivery: Option<JobTaskDelivery>,
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
    /// Last automated look at a failed run [ORB-13907]: invoked at most once
    /// per run, after step recovery is exhausted and before
    /// `failure_activity`. It returns one typed decision: `resume` (reruns the
    /// run from an earlier step), a settled decision (`complete_no_diff`,
    /// `reject`, `archive`, or `requeue`, which does not run
    /// `failure_activity`), or `escalate` (which, like a missing or malformed
    /// result, still runs `failure_activity`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_recovery_activity: Option<String>,
    #[serde(skip)]
    pub resolved_final_recovery_activity: Option<ActivityV2>,
    /// Mid-run steering's promotion gate: with
    /// `workflow.mid_run_steering.enabled`, the run does not start the
    /// `before` step while an operator comment past its steering watermark
    /// is undelivered, and reruns from `round` instead, a bounded number of
    /// times. Absent, or with steering off, no gate runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steering_gate: Option<SteeringGateSpec>,
    /// Runs of this job allowed to execute at once; later submissions wait
    /// `pending` in submission order. `0` imposes no job-level ceiling, for a
    /// job whose callers already bound how many runs they start.
    #[serde(default = "default_max_active_runs")]
    pub max_active_runs: u32,
    #[serde(default)]
    pub kind: JobKind,
    pub steps: Vec<JobV2Step>,
}

fn is_false(value: &bool) -> bool {
    !value
}

/// A job's `spec.steering_gate` declaration
/// (`docs/design/mid-run-steering/2_design.md` §3).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SteeringGateSpec {
    /// The top-level step the gate runs before, while the candidate is still
    /// uncommitted in its worktree.
    pub before: String,
    /// The earlier top-level step a steering round runs again, even when its
    /// `when:` is false, so the agent step there is dispatched once more
    /// with the steering block.
    pub round: String,
    /// The earlier top-level step whose output names a carried candidate's
    /// provenance. When it reports `implement: false`, the run carries the
    /// candidate of its `source_run_id` past its implementer, so that run's
    /// steering watermark is the gate's baseline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate: Option<String>,
}

/// A job's `spec.task_delivery` declaration.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct JobTaskDelivery {
    /// Ship modes a task may select this job for with a `delivery:<job>` tag.
    /// Empty for a job that holds the slot without being selectable: the ship
    /// coordinators, and the claimed leaves a claim binding selects.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modes: Vec<ShipMode>,
}

impl JobV2 {
    /// Whether a live run of this job holds its tasks' delivery slot.
    pub fn holds_task_delivery(&self) -> bool {
        self.task_delivery.is_some()
    }

    /// Whether a task may select this job to deliver it in `mode`.
    pub fn delivers_mode(&self, mode: ShipMode) -> bool {
        self.task_delivery
            .as_ref()
            .is_some_and(|delivery| delivery.modes.contains(&mode))
    }

    /// Every step in file order — a step, then the steps nested in its
    /// `parallel` / `fan_out` / `loop` body — paired with its effective
    /// stage. A step without `stage:` inherits the nearest earlier step's
    /// stage in its own list; a nested list starts from its parent step's
    /// effective stage, and a stage declared inside a nested body does not
    /// leak back to the steps after the parent.
    pub fn step_stages(&self) -> Vec<(&str, Option<&JobStage>)> {
        fn walk<'a>(
            steps: &'a [JobV2Step],
            inherited: Option<&'a JobStage>,
            out: &mut Vec<(&'a str, Option<&'a JobStage>)>,
        ) {
            let mut current = inherited;
            for step in steps {
                let effective = step.stage.as_ref().or(current);
                current = effective;
                out.push((step.id.as_str(), effective));
                match &step.body {
                    JobV2StepBody::Parallel { parallel } => {
                        walk(&parallel.branches, effective, out);
                    }
                    JobV2StepBody::FanOut { fan_out, .. } => {
                        walk(std::slice::from_ref(&*fan_out.worker), effective, out);
                    }
                    JobV2StepBody::Loop { loop_ } => walk(&loop_.steps, effective, out),
                    JobV2StepBody::TargetRef(_) | JobV2StepBody::Target(_) => {}
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.steps, None, &mut out);
        out
    }

    /// The stages this job declares, in first-appearance order. A stage id
    /// declared again keeps its first label. Empty for a job with no
    /// `stage:` keys.
    pub fn ordered_stages(&self) -> Vec<&JobStage> {
        let mut stages: Vec<&JobStage> = Vec::new();
        for (_, stage) in self.step_stages() {
            if let Some(stage) = stage
                && !stages.iter().any(|seen| seen.id == stage.id)
            {
                stages.push(stage);
            }
        }
        stages
    }

    /// The effective stage of the step with `step_id`, when the job has
    /// that step and a stage covers it.
    pub fn stage_for_step(&self, step_id: &str) -> Option<&JobStage> {
        self.step_stages()
            .into_iter()
            .find(|(id, _)| *id == step_id)
            .and_then(|(_, stage)| stage)
    }
}

/// A display stage a job step belongs to, declared with `stage:` in job
/// YAML. The short form is a bare id (`stage: implement`); the long form
/// adds a display label (`stage: { id: landing_review, label: Landing
/// review }`). Stages are display metadata only: execution ignores them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobStage {
    pub id: String,
    pub label: Option<String>,
}

impl JobStage {
    /// The text a stage indicator shows: the declared label, or the id.
    pub fn display_label(&self) -> &str {
        self.label.as_deref().unwrap_or(&self.id)
    }

    fn validate(id: &str, label: Option<&str>) -> Result<(), String> {
        let id_ok = !id.is_empty()
            && id.len() <= 64
            && id.as_bytes()[0].is_ascii_lowercase()
            && id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-');
        if !id_ok {
            return Err(format!(
                "invalid stage id `{id}` (want a lowercase letter then lowercase letters, digits, `_` or `-`, at most 64 bytes)"
            ));
        }
        if let Some(label) = label
            && (label.trim().is_empty() || label.len() > 80)
        {
            return Err("stage label must be non-empty and at most 80 bytes".to_string());
        }
        Ok(())
    }

    fn from_value(value: &Value) -> Result<Self, String> {
        let stage = match value {
            Value::String(id) => JobStage {
                id: id.clone(),
                label: None,
            },
            Value::Object(object) => {
                for key in object.keys() {
                    if key != "id" && key != "label" {
                        return Err(format!("stage declares unknown key `{key}`"));
                    }
                }
                let id = object
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "stage mapping missing string `id`".to_string())?
                    .to_string();
                let label = match object.get("label") {
                    None | Some(Value::Null) => None,
                    Some(Value::String(label)) => Some(label.clone()),
                    Some(_) => return Err("stage `label` must be a string".to_string()),
                };
                JobStage { id, label }
            }
            _ => {
                return Err(
                    "stage must be a stage id string or a mapping with `id` and optional `label`"
                        .to_string(),
                );
            }
        };
        JobStage::validate(&stage.id, stage.label.as_deref())?;
        Ok(stage)
    }
}

impl Serialize for JobStage {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match &self.label {
            None => serializer.serialize_str(&self.id),
            Some(label) => {
                use serde::ser::SerializeMap as _;
                let mut map = serializer.serialize_map(Some(2))?;
                map.serialize_entry("id", &self.id)?;
                map.serialize_entry("label", label)?;
                map.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for JobStage {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        JobStage::from_value(&value).map_err(serde::de::Error::custom)
    }
}

/// A step in a v2 job. Carries `id`, optional `when` / `retry` modifiers,
/// and exactly one body (target / parallel / fan_out / loop).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct JobV2Step {
    pub id: String,
    /// Display stage this step starts. A step without one inherits the
    /// nearest earlier step's stage; a nested step inherits its parent's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage: Option<JobStage>,
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
/// sees `Target` after `resolve_job_target_refs` runs at
/// load time. A `TargetRef` that survives into dispatch is a caller bug —
/// the job executor should never have to look up an activity by name.
#[derive(Debug, Clone, Serialize, PartialEq)]
// The resolved Target carries the activity snapshot inline in persisted job
// steps. Boxing it would churn every executor reader for a size heuristic;
// the additive optional program policy makes this enum cross that threshold.
#[allow(clippy::large_enum_variant)]
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
        let stage = match obj.remove("stage") {
            Some(value) => Some(
                JobStage::from_value(&value)
                    .map_err(|error| D::Error::custom(format!("step `{id}`: stage: {error}")))?,
            ),
            None => None,
        };
        let when = deserialize_optional_string::<D::Error>("when", obj.remove("when"))?;
        let retry = match obj.remove("retry") {
            Some(rv) => Some(
                serde_json::from_value::<RetrySpec>(rv)
                    .map_err(|e| D::Error::custom(format!("retry: {e}")))?,
            ),
            None => None,
        };
        let recovery_activity = deserialize_optional_string::<D::Error>(
            "recovery_activity",
            obj.remove("recovery_activity"),
        )?;

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
                let block = obj.remove("parallel").ok_or_else(|| {
                    D::Error::custom("step body key `parallel` disappeared during dispatch")
                })?;
                JobV2StepBody::Parallel {
                    parallel: serde_json::from_value(block)
                        .map_err(|e| D::Error::custom(format!("parallel: {e}")))?,
                }
            }
            (false, true, false, false, false) => {
                let fan_out_block = obj.remove("fan_out").ok_or_else(|| {
                    D::Error::custom("step body key `fan_out` disappeared during dispatch")
                })?;
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
                let block = obj.remove("loop").ok_or_else(|| {
                    D::Error::custom("step body key `loop` disappeared during dispatch")
                })?;
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
            _ => {
                return Err(D::Error::custom(
                    "step must set exactly one body shape: `target`, `spec`, `parallel`, `fan_out`, or `loop`",
                ));
            }
        };

        Ok(JobV2Step {
            id,
            stage,
            when,
            retry,
            recovery_activity,
            resolved_recovery_activity: None,
            body,
        })
    }
}

fn deserialize_optional_string<E: serde::de::Error>(
    field: &str,
    value: Option<Value>,
) -> Result<Option<String>, E> {
    match value {
        None => Ok(None),
        Some(value) => serde_json::from_value::<Option<String>>(value)
            .map_err(|error| E::custom(format!("`{field}`: {error}"))),
    }
}

/// Flat target step: inlines an `ActivityV2Spec` directly on the step. This
/// is the shape the executor operates on — [`TargetRef`] is rewritten to
/// `TargetStep` by `resolve_job_target_refs` before
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
/// `resolve_job_target_refs` looks the name up in the
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
    /// Optional pipeline-context key for position-aligned worker errors.
    /// Successful workers have null entries; failed workers have error strings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub errors: Option<String>,
}

/// Loop block (§4.2). Bounded iteration with optional value-break and a shared
/// Session map across iterations.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LoopBlock {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub items: Option<String>,
    pub max_iterations: u32,
    /// Optional elapsed budget in seconds, rendered once at loop entry.
    /// Zero runs no iterations; an in-flight body is not interrupted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<String>,
    /// Optional delay between unsuccessful iterations, capped by the budget.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval_seconds: Option<String>,
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
