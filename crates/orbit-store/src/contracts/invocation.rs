use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use orbit_types::telemetry::InvocationTrace;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct InvocationQuery {
    /// Only successful job runs that committed a candidate, with this
    /// invocation the final one inside a successful activity step's execution interval.
    /// Missing outcome evidence, no-diff runs and earlier retry attempts are excluded.
    #[serde(default)]
    pub succeeded_candidate_only: bool,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    /// Workspace the invocations were recorded under. Run ids are minted per
    /// workspace, so a `job_run_id` filter only names one run together with
    /// this. Rows recorded before invocations carried a workspace, and whose
    /// run could not be attributed to exactly one workspace, never match.
    pub workspace_id: Option<String>,
    pub job_run_id: Option<String>,
    pub activity_id: Option<String>,
    pub task_id: Option<String>,
    pub agent: Option<String>,
    pub model: Option<String>,
    pub tool_name: Option<String>,
    pub limit: usize,
    /// Leave each record's `tool_calls` empty instead of loading them, for
    /// callers that read only the invocation rows and their task links.
    #[serde(default)]
    pub skip_tool_calls: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InvocationInsertParams {
    /// Dispatch record captured before launch; absent for historical callers.
    #[serde(default)]
    pub agent_execution_id: Option<i64>,
    pub job_run_id: String,
    pub activity_id: String,
    pub agent: String,
    /// The canonical provider the invocation ran on, which `agent` does not
    /// name for every lane: an Antigravity run is attributed to the model's
    /// family. `None` for a caller that does not know it [ORB-14699].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// The crew account the invocation ran under: a crew that sets `env` and
    /// so runs on its own credential, whose budget reads only its spend.
    /// `None` for the provider's default login [ORB-17288].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    pub model: Option<String>,
    pub task_ids: Vec<String>,
    pub trace: InvocationTrace,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InvocationToolCallRecord {
    pub invocation_id: i64,
    pub seq: u64,
    pub tool_name: String,
    pub result_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InvocationRecord {
    /// Actual dispatch identity. None means historical effort is unknown.
    #[serde(default)]
    pub execution: Option<orbit_types::telemetry::AgentExecutionIdentity>,
    pub id: i64,
    pub ts: DateTime<Utc>,
    pub job_run_id: String,
    pub activity_id: String,
    pub agent: String,
    /// Recorded provider, or the dispatch provider when completion omitted it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    pub model: Option<String>,
    pub duration_ms: u64,
    pub input_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_create_tokens: u64,
    /// Premium 1-hour-TTL cache-creation tokens (`TokenUsage::cache_create_1h`).
    pub cache_create_1h_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub tool_call_count: u64,
    pub task_ids: Vec<String>,
    pub tool_calls: Vec<InvocationToolCallRecord>,
    /// The provider's output outgrew the bounded capture, so `tool_calls`
    /// holds only the suffix that survived it (`InvocationTrace::tool_calls_incomplete`).
    /// A first edit or an absent tool read from it is unproven.
    #[serde(default)]
    pub tool_calls_incomplete: bool,
    /// Provider-reported total cost in USD, persisted verbatim from
    /// [`InvocationInsertParams::trace`] for monthly manual reconciliation.
    /// Never overwritten by `derived_cost_usd`.
    pub provider_cost_usd: Option<f64>,
    /// Normalized cost in USD derived at query time from `model`, `ts`, and
    /// the token splits against the versioned price table
    /// (`orbit_common::model::pricing`). `None` when no price row covers
    /// this model/date.
    pub derived_cost_usd: Option<f64>,
    /// The crew account the invocation ran under, when its crew sets `env`;
    /// `None` for the provider's default login [ORB-17288].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
}

/// Workspace and date window for reconciliation-safe invocation accounting reads.
///
/// `until` is always exclusive. Callers capture it before loading so rows
/// arriving during aggregation cannot make one read internally inconsistent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InvocationAccountingQuery {
    /// Restricts facts to the workspace they were recorded under. `None`
    /// retains host-wide reads; legacy rows without an attributed workspace
    /// never match a workspace-scoped read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    pub since: Option<DateTime<Utc>>,
    pub until: DateTime<Utc>,
}

/// One invocation and its distinct task linkage, without detailed tool calls.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InvocationAccountingFact {
    /// Actual dispatch identity. None means historical effort is unknown.
    #[serde(default)]
    pub execution: Option<orbit_types::telemetry::AgentExecutionIdentity>,
    pub id: i64,
    /// Run identity within the queried workspace; absent on old serialized facts.
    #[serde(default)]
    pub job_run_id: String,
    pub ts: DateTime<Utc>,
    pub model: Option<String>,
    pub input_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_create_tokens: u64,
    pub cache_create_1h_tokens: u64,
    pub output_tokens: u64,
    pub task_ids: Vec<String>,
    pub provider_cost_usd: Option<f64>,
    pub derived_cost_usd: Option<f64>,
    /// The pipeline step (or activity) the invocation ran under.
    #[serde(default)]
    pub activity_id: Option<String>,
    /// Wall time the invocation ran; `ts` is recorded when it ended.
    #[serde(default)]
    pub duration_ms: u64,
}

/// One invocation's spend as a provider budget reads it from the host ledger
/// [ORB-14699].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProviderLedgerEntry {
    pub ts: DateTime<Utc>,
    /// Input plus output tokens, as the ledger totals an invocation.
    pub tokens: u64,
    /// The provider-reported cost; `None` when the invocation recorded none.
    pub cost_usd: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ActivityInvocationMetrics {
    pub activity_id: String,
    pub agent: String,
    pub model: Option<String>,
    pub invocation_count: u64,
    pub total_input_tokens: u64,
    pub total_cache_read_tokens: u64,
    pub total_cache_create_tokens: u64,
    pub total_output_tokens: u64,
    pub total_tokens: u64,
    pub avg_tokens: f64,
    pub p50_tokens: u64,
    pub p95_tokens: u64,
    pub total_tool_calls: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskInvocationMetrics {
    pub task_id: String,
    pub invocation_count: u64,
    pub total_input_tokens: u64,
    pub total_cache_read_tokens: u64,
    pub total_cache_create_tokens: u64,
    pub total_output_tokens: u64,
    pub total_tokens: u64,
    pub total_tool_calls: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolInvocationMetrics {
    pub activity_id: String,
    pub tool_name: String,
    pub call_count: u64,
    pub avg_result_bytes: f64,
    pub total_result_bytes: u64,
}

/// One agent launch, retained independently of its completion telemetry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentExecutionRecord {
    pub id: i64,
    pub run_id: String,
    pub activity_id: String,
    pub provider: String,
    pub model: Option<String>,
    pub identity: orbit_types::telemetry::AgentExecutionIdentity,
    pub started_at: DateTime<Utc>,
}

/// Aggregate completed invocations without consulting mutable crew configuration.
/// Missing dispatch identity is a separate historical `unknown` population.
pub fn agent_effort_usage<'a>(
    facts: impl IntoIterator<Item = &'a InvocationAccountingFact>,
) -> Vec<orbit_types::telemetry::AgentEffortUsage> {
    let mut accumulator = AgentEffortUsageAccumulator::default();
    for fact in facts {
        record_agent_effort_usage(&mut accumulator, fact);
    }
    finish_agent_effort_usage(accumulator)
}

type AgentEffortKey = (Option<String>, Option<String>, Option<String>);
type AgentEffortPopulation = (
    orbit_types::telemetry::AgentEffortUsage,
    std::collections::BTreeSet<String>,
);

/// Bounded by distinct crew/model/effort identities and linked task ids, without
/// retaining another copy of the full invocation population.
#[derive(Debug, Default)]
pub struct AgentEffortUsageAccumulator {
    rows: std::collections::BTreeMap<AgentEffortKey, AgentEffortPopulation>,
}

/// Add exactly one completed invocation to its recorded population.
pub fn record_agent_effort_usage(
    accumulator: &mut AgentEffortUsageAccumulator,
    fact: &InvocationAccountingFact,
) {
    use orbit_types::telemetry::AgentEffortUsage;
    let crew = fact
        .execution
        .as_ref()
        .and_then(|execution| execution.crew.clone());
    let effort = fact
        .execution
        .as_ref()
        .map(|execution| execution.reasoning_effort.map(|effort| effort.to_string()))
        .unwrap_or(Some("unknown".to_string()));
    let (row, tasks) = accumulator
        .rows
        .entry((crew.clone(), fact.model.clone(), effort.clone()))
        .or_insert_with(|| {
            (
                AgentEffortUsage {
                    crew,
                    model: fact.model.clone(),
                    reasoning_effort: effort,
                    invocations: 0,
                    tasks: 0,
                    duration_ms: 0,
                    input_tokens: 0,
                    output_tokens: 0,
                    provider_cost_usd: 0.0,
                    provider_cost_count: 0,
                },
                std::collections::BTreeSet::new(),
            )
        });
    row.invocations += 1;
    row.duration_ms = row.duration_ms.saturating_add(fact.duration_ms);
    row.input_tokens = row.input_tokens.saturating_add(fact.input_tokens);
    row.output_tokens = row.output_tokens.saturating_add(fact.output_tokens);
    if let Some(cost) = fact
        .provider_cost_usd
        .filter(|cost| cost.is_finite() && *cost >= 0.0)
    {
        row.provider_cost_usd += cost;
        row.provider_cost_count += 1;
    }
    tasks.extend(fact.task_ids.iter().cloned());
}
/// Stable identity order, with distinct task support for each population.
pub fn finish_agent_effort_usage(
    accumulator: AgentEffortUsageAccumulator,
) -> Vec<orbit_types::telemetry::AgentEffortUsage> {
    accumulator
        .rows
        .into_values()
        .map(|(mut row, tasks)| {
            row.tasks = tasks.len() as u64;
            row
        })
        .collect()
}
