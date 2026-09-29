use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenUsage {
    #[serde(default)]
    pub input: u64,
    #[serde(default)]
    pub cache_read: u64,
    /// Cache-creation (write) tokens billed at the standard 5-minute-TTL rate.
    /// Providers that report a single cache-creation counter (or that don't
    /// distinguish TTLs) fold everything here.
    #[serde(default)]
    pub cache_create: u64,
    /// Cache-creation (write) tokens billed at the premium 1-hour-TTL rate
    /// (Anthropic `ephemeral_1h_input_tokens`). Kept separate from
    /// [`Self::cache_create`] so the price table can charge the 1h rate (2x
    /// input) distinctly from the 5m rate (1.25x input). Zero when the provider
    /// doesn't split TTLs.
    #[serde(default)]
    pub cache_create_1h: u64,
    #[serde(default)]
    pub output: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ToolCallTrace {
    #[serde(default)]
    pub seq: u32,
    #[serde(default)]
    pub tool_name: String,
    #[serde(default)]
    pub result_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_payload: Option<Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct InvocationTrace {
    #[serde(default)]
    pub usage: TokenUsage,
    #[serde(default)]
    pub tool_calls: Vec<ToolCallTrace>,
    #[serde(default)]
    pub duration_ms: u64,
    /// Exact model string reported by the provider CLI, when its result
    /// protocol identifies one unambiguously. This is deliberately opaque:
    /// aliases and provider-specific suffixes are preserved verbatim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_model: Option<String>,
    /// Provider-reported total cost in USD for this invocation, when the
    /// caller has it (e.g. Claude CLI `total_cost_usd`). Kept verbatim
    /// alongside the token-derived cost computed from the price table at
    /// query time — it never overwrites the derived figure and is the
    /// ground truth for monthly manual reconciliation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_cost_usd: Option<f64>,
}
