use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use orbit_types::plugin::{PluginProvenance, PluginSecretUpdateStatus};
use orbit_types::telemetry::{AuditAttribution, AuditEventStatus};
use orbit_types::tool::{McpCapability, McpTransport};

#[derive(Debug, Clone)]
pub struct AuditEventInsertParams {
    pub execution_id: String,
    pub command: String,
    pub subcommand: Option<String>,
    pub tool_name: Option<String>,
    pub target_type: Option<String>,
    pub target_id: Option<String>,
    pub role: String,
    pub status: AuditEventStatus,
    pub exit_code: i32,
    pub duration_ms: i64,
    pub working_directory: String,
    pub arguments_json: Option<String>,
    pub stdout_truncated: Option<String>,
    pub stderr_truncated: Option<String>,
    pub error_message: Option<String>,
    pub host: Option<String>,
    pub pid: u32,
    pub session_id: Option<String>,
    pub workspace_id: Option<String>,
    pub caller_machine_id: Option<String>,
    pub caller_machine_name: Option<String>,
    pub process_machine_id: Option<String>,
    pub process_machine_name: Option<String>,
    pub transport: Option<McpTransport>,
    pub effective_capabilities: BTreeSet<McpCapability>,
    pub origin_session_id: Option<String>,
    pub mcp_call_id: Option<String>,
    pub lease_id: Option<String>,
    pub task_id: Option<String>,
    pub job_run_id: Option<String>,
    pub activity_id: Option<String>,
    pub step_index: Option<i64>,
}

#[derive(Debug, Clone, Default)]
pub struct AuditEventFilter {
    pub since: Option<DateTime<Utc>>,
    pub tool_name: Option<String>,
    pub target_type: Option<String>,
    pub status: Option<AuditEventStatus>,
    pub role: Option<String>,
    pub workspace_id: Option<String>,
    pub caller_machine_id: Option<String>,
    pub process_machine_id: Option<String>,
    pub transport: Option<McpTransport>,
    pub capability: Option<McpCapability>,
    pub origin_session_id: Option<String>,
    pub mcp_call_id: Option<String>,
    pub job_run_id: Option<String>,
    pub lease_id: Option<String>,
    pub limit: usize,
    /// Rows to skip after ordering (newest first). Pushed into SQL so a
    /// caller can page past the first `limit` rows without prefetching the
    /// entire history in front of the page it wants.
    pub offset: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditToolCallCountsByRole {
    pub role: String,
    pub total: u64,
    pub failed: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditToolCallCountsBySurfaceAndRole {
    pub surface: String,
    pub role: String,
    pub total: u64,
    pub failed: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditTopToolCall {
    pub role: String,
    pub tool_name: String,
    pub total: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AuditToolAggregate {
    pub tool_name: String,
    pub total: i64,
    pub successes: i64,
    pub failures: i64,
    pub denials: i64,
    pub mcp_total: i64,
    pub cli_total: i64,
    pub mcp_failures: i64,
    pub cli_failures: i64,
    pub avg_duration_ms: f64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditRoleAggregate {
    pub role: String,
    /// All audit events in the window, regardless of subcommand. Equal to
    /// `mcp + cli + other + no_subcommand`.
    pub total: i64,
    /// Tool invocations via MCP (`subcommand = 'run-mcp'`).
    pub mcp: i64,
    /// Tool invocations via CLI (`subcommand = 'run'`).
    pub cli: i64,
    /// Non-tool CLI subcommands (e.g. `show`, `list`) — present and not `run`/`run-mcp`.
    pub other: i64,
    /// Internal/system events with no subcommand at all (e.g. lock reservations).
    pub no_subcommand: i64,
}

/// Per-canonical-actor aggregate of audit events (ORB-10888).
///
/// The grouping key is `(kind, actor)`, so every granularity of one agent —
/// `claude`, `opus`, `claude-opus-5` — lands in a single row. `model` is
/// deliberately absent from the key: it is the finer grain that the raw
/// `role`-grouped aggregate already splits on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditActorAggregate {
    /// [`orbit_types::telemetry::ActorKind`] as its wire string.
    pub kind: String,
    /// Canonical grouping key within `kind`: the agent family for agents, the
    /// canonical label otherwise.
    pub actor: String,
    pub vendor: Option<String>,
    pub family: Option<String>,
    pub total: i64,
    pub mcp: i64,
    pub cli: i64,
}

/// Transport-neutral invocation facts a tool session contributes to its audit
/// row, alongside the legacy [`AuditEventInsertParams`] DTO.
///
/// These stay off the DTO so the ~two dozen non-tool audit producers do not
/// have to name a field that can only ever be `None` for them.
#[derive(Debug, Clone, Copy, Default)]
pub struct AuditInvocationFields<'a> {
    /// Per-invocation correlation ID minted by the accepting process.
    pub trace_id: Option<&'a str>,
    /// Caller network address observed by the accepting process. Audit
    /// metadata, not caller identity.
    pub caller_ip: Option<&'a str>,
    /// Identity the caller claimed for itself, already normalized by
    /// [`orbit_types::telemetry::normalize_self_reported_actor`] [ORB-10890].
    ///
    /// Written to its own column and nothing else. It never contributes to
    /// [`AuditEventInsertParams::role`] or to the `actor_*` projection derived
    /// from it, so no query that reads trusted identity can pick it up.
    pub self_reported_actor: Option<&'a str>,
    /// The plugin behind this tool call: name, version, and manifest digest
    /// (design `docs/design/plugins/1_scope.md` §4.4). `None` for every
    /// built-in and legacy external tool.
    pub plugin: Option<&'a PluginProvenance>,
    /// The names of the declared secrets the plugin call's request carried
    /// (design `docs/design/plugins/1_scope.md` §3, "Plugin secrets"). Names
    /// only: no value reaches an audit row. Empty for every other call.
    pub plugin_secrets: &'a [String],
    /// Each secret the plugin's backend asked to rotate, by name, and whether
    /// the update was applied or refused (design
    /// `docs/design/plugins/1_scope.md` §3, "Plugin secrets"). `None`, or an
    /// empty map, for every call that rotated nothing.
    pub plugin_secret_updates: Option<&'a BTreeMap<String, PluginSecretUpdateStatus>>,
}

/// Per-(actor, attribution) aggregate of audited tool calls [ORB-10890].
///
/// Every tool-call row in the window lands in exactly one bucket, so the
/// caller can read authenticated-only, self-reported-only, and combined
/// denominators off the same result set without inferring one from another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditAttributionAggregate {
    /// How this row's identity was established.
    pub attribution: AuditAttribution,
    /// The actor label for this bucket: the canonical actor for
    /// [`AuditAttribution::Authenticated`], the caller's own claim for
    /// [`AuditAttribution::SelfReported`], and
    /// [`orbit_types::telemetry::ANONYMOUS_ACTOR_LABEL`] otherwise.
    ///
    /// Two rows may carry the same `actor` under different `attribution`
    /// values. That is not a duplicate to be merged: it is one agent whose
    /// traffic Orbit could authenticate some of the time.
    pub actor: String,
    pub total: u64,
    /// Non-`success` rows (`failure` + `denied`), matching
    /// [`AuditToolCallCountsByRole::failed`].
    pub failed: u64,
    /// Tool invocations via MCP (`subcommand = 'run-mcp'`).
    pub mcp: u64,
    /// Tool invocations via CLI (`subcommand = 'run'`).
    pub cli: u64,
}
