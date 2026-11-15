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

/// Smallest prompt, in tokens, of a model request whose usage an invocation
/// trace keeps in [`InvocationTrace::long_prompt_requests`]. It is the lowest
/// threshold of any prompt-size price tier, so a request below it can never
/// reach one. The price table refuses a tier below it.
pub const LONG_PROMPT_CAPTURE_FLOOR_TOKENS: u64 = 100_000;

impl TokenUsage {
    /// Whether this one request's prompt may reach
    /// [`LONG_PROMPT_CAPTURE_FLOOR_TOKENS`]. The prompt is bounded from above
    /// by every input-side bucket: exact where input excludes the cache
    /// buckets, an overcount where a provider's input total already includes
    /// them, so no request that reaches the floor is dropped.
    #[must_use]
    pub fn may_reach_long_prompt_floor(&self) -> bool {
        self.input
            .saturating_add(self.cache_read)
            .saturating_add(self.cache_create)
            .saturating_add(self.cache_create_1h)
            >= LONG_PROMPT_CAPTURE_FLOOR_TOKENS
    }
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
    /// The provider's stdout outgrew the bounded capture, so `tool_calls`
    /// holds only the calls that survived in its retained suffix, numbered
    /// from 1 within it. Calls before that suffix are lost, so the list can
    /// neither prove that a tool was never called nor locate a first edit.
    /// `false` for a complete trace and for every invocation recorded before
    /// the flag existed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub tool_calls_incomplete: bool,
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
    /// Usage of each model request whose prompt may reach a prompt-size price
    /// tier (see [`TokenUsage::may_reach_long_prompt_floor`]), one entry per
    /// request, in the same buckets as [`Self::usage`]. Prompt-size tiers price
    /// a whole request by its own prompt, so invocation totals cannot key them.
    ///
    /// `None` means the provider output exposed no per-request usage (and
    /// every invocation recorded before it was captured); `Some` with no
    /// entries means every request stayed below the floor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub long_prompt_requests: Option<Vec<TokenUsage>>,
}

/// The immutable identity actually rendered to an agent provider. An absent
/// identity on a historical invocation means unknown, while a present identity
/// with null effort means the provider default was used.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct AgentExecutionIdentity {
    /// Resolved crew at dispatch, absent for an inline agent without a crew.
    pub crew: Option<String>,
    /// Effective provider-mapped effort; null when no effort was rendered.
    pub reasoning_effort: Option<crate::identity::ReasoningEffort>,
}

/// Completed invocation usage grouped by the actual crew, model and effort.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentEffortUsage {
    pub crew: Option<String>,
    pub model: Option<String>,
    /// `unknown` for historical invocations, null for the provider default.
    pub reasoning_effort: Option<String>,
    pub invocations: u64,
    pub tasks: u64,
    pub duration_ms: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub provider_cost_usd: f64,
    pub provider_cost_count: u64,
}
