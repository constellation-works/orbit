use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use orbit_types::telemetry::InvocationTrace;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct InvocationQuery {
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
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InvocationInsertParams {
    pub job_run_id: String,
    pub activity_id: String,
    pub agent: String,
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
    pub id: i64,
    pub ts: DateTime<Utc>,
    pub job_run_id: String,
    pub activity_id: String,
    pub agent: String,
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
    /// Provider-reported total cost in USD, persisted verbatim from
    /// [`InvocationInsertParams::trace`] for monthly manual reconciliation.
    /// Never overwritten by `derived_cost_usd`.
    pub provider_cost_usd: Option<f64>,
    /// Normalized cost in USD derived at query time from `model`, `ts`, and
    /// the token splits against the versioned price table
    /// (`orbit_common::model::pricing`). `None` when no price row covers
    /// this model/date.
    pub derived_cost_usd: Option<f64>,
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
    pub id: i64,
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
pub struct AgentInvocationMetrics {
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
