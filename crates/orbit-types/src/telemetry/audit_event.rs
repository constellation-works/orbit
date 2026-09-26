use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{Display, Formatter};
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::plugin::{PluginProvenance, PluginSecretUpdateStatus};
use crate::telemetry::audit_actor::{CanonicalActor, canonical_actor_for_role_label};
use crate::tool::{McpCapability, McpTransport};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[serde(rename_all = "snake_case")]
pub enum AuditEventStatus {
    Success,
    Failure,
    Denied,
}

impl Display for AuditEventStatus {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            AuditEventStatus::Success => write!(f, "success"),
            AuditEventStatus::Failure => write!(f, "failure"),
            AuditEventStatus::Denied => write!(f, "denied"),
        }
    }
}

impl FromStr for AuditEventStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "success" => Ok(AuditEventStatus::Success),
            "failure" => Ok(AuditEventStatus::Failure),
            "denied" => Ok(AuditEventStatus::Denied),
            other => Err(format!("unknown audit event status: {other}")),
        }
    }
}

/// A comprehensive, persistent audit trail record for a CLI command execution.
/// Stored in SQLite and exposed via `orbit audit list` / `orbit audit show`.
/// Captures execution context including timing, exit code, role, tool name, and
/// truncated stdout/stderr for post-hoc review.
///
/// Contrast with [`Audit`](crate::record::Audit), which is the lightweight in-memory
/// event log entry produced by the runtime for internal observability.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AuditEvent {
    pub id: i64,
    pub execution_id: String,
    pub timestamp: DateTime<Utc>,
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
    /// Stable logical workspace identity after trusted adapter/runtime
    /// resolution. The legacy working directory remains unchanged.
    #[serde(default)]
    pub workspace_id: Option<String>,
    #[serde(default)]
    pub caller_machine_id: Option<String>,
    /// [ORB-12725] `caller_host_id` is read for one release so an audit
    /// export written by an older build still loads.
    #[serde(default, alias = "caller_host_id")]
    pub caller_machine_name: Option<String>,
    #[serde(default)]
    pub process_machine_id: Option<String>,
    /// [ORB-12725] `process_host_id` is read for one release; see
    /// [`Self::caller_machine_name`].
    #[serde(default, alias = "process_host_id")]
    pub process_machine_name: Option<String>,
    #[serde(default)]
    pub transport: Option<McpTransport>,
    /// Complete effective MCP capability set, canonically ordered by the
    /// shared enum's `Ord` implementation.
    #[serde(default)]
    pub effective_capabilities: BTreeSet<McpCapability>,
    #[serde(default)]
    pub origin_session_id: Option<String>,
    #[serde(default)]
    pub mcp_call_id: Option<String>,
    /// Transport-neutral per-invocation correlation identifier. Legacy rows
    /// and producers that predate the common invocation envelope leave this
    /// unset.
    #[serde(default)]
    pub trace_id: Option<String>,
    /// Caller network address observed by the accepting process, when the
    /// transport exposes one. This is audit metadata, not caller identity.
    #[serde(default)]
    pub caller_ip: Option<String>,
    #[serde(default)]
    pub lease_id: Option<String>,
    /// Orbit task ID the invocation was executed under, if known. CLI retains
    /// its legacy input/env precedence; MCP accepts only authenticated managed
    /// envelope correlation and never model-authored tool JSON.
    #[serde(default)]
    pub task_id: Option<String>,
    /// Canonical job run ID. For MCP, a trusted leased-run ID fills an empty
    /// value or must match authenticated managed-envelope correlation.
    #[serde(default)]
    pub job_run_id: Option<String>,
    /// Activity name the invocation was executed under (e.g. `agent_implement`).
    #[serde(default)]
    pub activity_id: Option<String>,
    /// Zero-based step index within the enclosing job run, when known.
    #[serde(default)]
    pub step_index: Option<i64>,
    /// Identity the caller claimed for itself, recorded verbatim-after-
    /// normalization and **never authenticated** [ORB-10890].
    ///
    /// This is the only attribution an MCP client started from its own config
    /// can supply, and it is deliberately kept out of [`Self::role`] and the
    /// `actor_*` projection. `None` means the caller claimed nothing usable —
    /// anonymous, never inherited from anywhere else. Any surface that renders
    /// it must mark it unverified; see
    /// [`crate::telemetry::normalize_self_reported_actor`].
    #[serde(default)]
    pub self_reported_actor: Option<String>,
    /// The plugin that backed this tool call, when one did: name, version and
    /// manifest digest, written together by plugin dispatch [design
    /// `docs/design/plugins/1_scope.md` §4.4]. Absent for built-in tools and
    /// for every row written before the plugin standard.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin: Option<PluginProvenance>,
    /// The names of the declared secrets this plugin call's request carried
    /// (design `docs/design/plugins/1_scope.md` §3, "Plugin secrets"). Names
    /// only — a value is never written to an audit row. Empty for every
    /// other call and for rows written before secret delivery.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plugin_secrets: Vec<String>,
    /// Each secret the plugin's backend asked to rotate through
    /// `secret_updates`, by name, and whether the update was applied or
    /// refused (design `docs/design/plugins/1_scope.md` §3, "Plugin
    /// secrets"). Names and outcomes only — never a value. Empty for every
    /// call that rotated nothing and for rows written before rotation.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub plugin_secret_updates: BTreeMap<String, PluginSecretUpdateStatus>,
}

impl AuditEvent {
    /// Canonical actor identity for this event, derived from [`Self::role`].
    ///
    /// Derived rather than stored so a row always reads back under the current
    /// alias map. The persisted `actor_*` columns exist for SQL grouping and
    /// carry the alias version that produced them; see
    /// [`crate::telemetry::canonical_actor_for_role_label`].
    pub fn actor(&self) -> CanonicalActor {
        canonical_actor_for_role_label(&self.role)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AuditStats {
    pub total: i64,
    pub success_count: i64,
    pub failure_count: i64,
    pub denied_count: i64,
    pub avg_duration_ms: f64,
    pub p95_duration_ms: i64,
    pub max_duration_ms: i64,
}
