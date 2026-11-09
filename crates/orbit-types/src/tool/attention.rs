//! Public records of the read-only operator attention feed.
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Closed vocabulary of attention source conditions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttentionKind {
    /// The `pilot_warning` source condition.
    PilotWarning,
    /// The `pilot_hold` source condition.
    PilotHold,
    /// The `blocked` source condition.
    Blocked,
    /// The `decision_open` source condition.
    DecisionOpen,
    /// The `review_waiting` source condition.
    ReviewWaiting,
    /// The `run_failed` source condition.
    RunFailed,
    /// The `run_stuck` source condition.
    RunStuck,
    /// The `claim_stale` source condition.
    ClaimStale,
    /// The `ci_red` source condition.
    CiRed,
    /// The `friction_untriaged` source condition.
    FrictionUntriaged,
    /// The `host_unhealthy` source condition.
    HostUnhealthy,
    /// The `notice_open` source condition: an effective-open fleet notice.
    NoticeOpen,
    /// The `resident_stalled` source condition: an active resident mandate
    /// with no checkpoint for twice its heartbeat, or three `incomplete`
    /// cycles in a row.
    ResidentStalled,
}
impl AttentionKind {
    /// Stable wire names, including kinds implemented by later source slices.
    pub const NAMES: &'static [&'static str] = &[
        "pilot_warning",
        "pilot_hold",
        "blocked",
        "decision_open",
        "review_waiting",
        "run_failed",
        "run_stuck",
        "claim_stale",
        "ci_red",
        "friction_untriaged",
        "host_unhealthy",
        "notice_open",
        "resident_stalled",
    ];
    /// Kinds read when a call names none: the kinds this build derives.
    /// The others are accepted by name and report `degraded` until their
    /// sources land.
    pub const DEFAULT_NAMES: &'static [&'static str] = &[
        "pilot_warning",
        "pilot_hold",
        "blocked",
        "decision_open",
        "review_waiting",
        "run_failed",
        "run_stuck",
        "claim_stale",
        "ci_red",
        "friction_untriaged",
        "host_unhealthy",
        "notice_open",
        "resident_stalled",
    ];
    /// Kinds derived from one workspace's task, decision and review records,
    /// which only the workspace's owner host reports.
    pub const TASK_LEVEL_NAMES: &'static [&'static str] = &[
        "pilot_warning",
        "pilot_hold",
        "blocked",
        "decision_open",
        "review_waiting",
    ];
    /// Stable wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PilotWarning => "pilot_warning",
            Self::PilotHold => "pilot_hold",
            Self::Blocked => "blocked",
            Self::DecisionOpen => "decision_open",
            Self::ReviewWaiting => "review_waiting",
            Self::RunFailed => "run_failed",
            Self::RunStuck => "run_stuck",
            Self::ClaimStale => "claim_stale",
            Self::CiRed => "ci_red",
            Self::FrictionUntriaged => "friction_untriaged",
            Self::HostUnhealthy => "host_unhealthy",
            Self::NoticeOpen => "notice_open",
            Self::ResidentStalled => "resident_stalled",
        }
    }
}
/// Severity in increasing order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttentionSeverity {
    /// Informational operator wait.
    Low,
    /// A condition worth addressing.
    Medium,
    /// Work is held or the condition has aged.
    High,
    /// A condition stops other work.
    Critical,
}
/// Identity of an in-scope workspace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttentionWorkspace {
    /// Logical workspace id; empty (`null` on the wire) for an item about a
    /// host with no workspace, such as an unreachable destination.
    #[serde(with = "empty_as_null")]
    pub id: String,
    /// Registered name; empty (`null` on the wire) like `id`.
    #[serde(with = "empty_as_null")]
    pub name: String,
    /// Host-qualified selector, attached by the federated aggregator from the
    /// destination's live workspace list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector: Option<String>,
}
impl AttentionWorkspace {
    /// A workspace identity without a federated selector.
    pub fn local(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            selector: None,
        }
    }
    /// The `{id: null, name: null}` workspace of a host-level item.
    pub fn none() -> Self {
        Self::local("", "")
    }
}
mod empty_as_null {
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(value: &str, serializer: S) -> Result<S::Ok, S::Error> {
        if value.is_empty() {
            serializer.serialize_none()
        } else {
            serializer.serialize_str(value)
        }
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
        Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_default())
    }
}
/// Reachability of one host in a federated response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttentionHostStatus {
    /// Pinned machine id.
    pub machine_id: String,
    /// Configured display name.
    pub name: String,
    /// `reachable`, `unreachable_destination` or `tool_not_on_this_host`.
    pub reachability: String,
}
/// One failing or warning default `orbit doctor` workspace check, the local
/// `host_unhealthy` source.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttentionHealthCheck {
    /// Stable doctor check name.
    pub check: String,
    /// `warning` or `error`.
    pub status: String,
    /// The check's own message.
    pub message: String,
    /// The check's fix hint, when it names one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
}
/// Host whose authoritative state produced the item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttentionHost {
    /// Stable machine id.
    pub machine_id: String,
    /// Configured display name.
    pub name: String,
}
/// A suggested action; reading a feed never executes it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttentionNext {
    /// Imperative description.
    pub summary: String,
    /// Tool when the action has an MCP surface.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    /// Complete known input; arguments needing operator judgment are omitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
    /// Copyable CLI invocation, with placeholders for required operator input.
    pub command: String,
    /// Arguments the operator must provide.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub needs: Vec<String>,
    /// Alternative actions or inspection.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub also: Vec<AttentionNext>,
}
impl AttentionNext {
    /// Name a suggestion whose tool is a family operation by the family tool
    /// a caller invokes, with the operation's `action` in its input, here and
    /// in every alternative.
    pub fn name_tool_family(&mut self) {
        if let Some(tool) = self.tool.take() {
            let input = self
                .input
                .take()
                .unwrap_or_else(|| Value::Object(Default::default()));
            let (tool, input) = super::family::tool_family_call(&tool, input);
            self.tool = Some(tool);
            self.input = Some(input)
                .filter(|input| input.as_object().is_none_or(|object| !object.is_empty()));
        }
        for also in &mut self.also {
            also.name_tool_family();
        }
    }
}
/// Subject task metadata used by task-scoped filters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttentionTask {
    /// Current lifecycle state.
    pub status: crate::task::TaskStatus,
    /// Task tags.
    pub tags: Vec<String>,
    /// Assigned crew, omitted when unassigned.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub crew: Option<String>,
    /// Attributed orchestrator, omitted when unattributed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub orchestrator: Option<String>,
    /// Task title.
    pub title: String,
}
/// One currently actionable source condition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttentionItem {
    /// Stable identity independent of material state.
    pub id: String,
    /// Source condition.
    pub kind: AttentionKind,
    /// Derived urgency.
    pub severity: AttentionSeverity,
    /// Authoritative workspace.
    pub workspace: AttentionWorkspace,
    /// Authoritative host.
    pub host: AttentionHost,
    /// Subject references.
    pub refs: Value,
    /// Subject metadata, absent for non-task items.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<AttentionTask>,
    /// Condition onset.
    pub since: Option<chrono::DateTime<chrono::Utc>>,
    /// Age at generation, absent when onset is unknown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub age_seconds: Option<u64>,
    /// Attached decision expiration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Bounded one-line quotation of the source.
    pub reason: String,
    /// Bounded kind-specific context.
    pub detail: Value,
    /// Suggested resolution.
    pub next: AttentionNext,
    /// Material-state digest, excluding elapsed time.
    pub fingerprint: String,
}
/// Why a source could not supply complete candidates.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttentionDegraded {
    /// Workspace or host scope.
    pub scope: String,
    /// Affected kind.
    pub kind: String,
    /// Stable degradation code.
    pub code: String,
    /// Source error or capability explanation.
    pub message: String,
}
/// Delta cursor disposition; unusable tokens always restart the full list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttentionCursorStatus {
    /// No token supplied.
    None,
    /// A valid token selected a delta.
    Accepted,
    /// Token exceeded its TTL.
    Expired,
    /// Malformed or future token.
    Invalid,
    /// Unknown format version.
    Unsupported,
}

/// Counts for the public page after source caps and delta selection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttentionCounts {
    /// Matching delta items before the public page limit.
    pub matching: usize,
    /// Items in this public page.
    pub returned: usize,
    /// Whether the public page limit omitted unseen items.
    pub truncated: bool,
    /// Returned items by kind.
    pub by_kind: std::collections::BTreeMap<AttentionKind, usize>,
    /// Returned items by severity.
    pub by_severity: std::collections::BTreeMap<AttentionSeverity, usize>,
    /// Matching items already seen; present only in accepted delta mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unchanged: Option<usize>,
}
/// A stateless continuation token and its disposition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttentionCursor {
    /// Opaque token for the next read.
    pub next: String,
    /// Disposition of the supplied token.
    pub status: AttentionCursorStatus,
    /// Present when the seen set exceeded the protocol cap.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncated: Option<bool>,
}
/// Public response of the local attention operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttentionEnvelope {
    /// Wire schema version.
    pub schema_version: u32,
    /// Shared evaluation time for this response.
    pub generated_at: chrono::DateTime<chrono::Utc>,
    /// `true` only on the federated aggregator's candidate read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_read: Option<bool>,
    /// Echoed input filters with resolved workspace identities.
    pub filters: Value,
    /// The ordered public page.
    pub items: Vec<AttentionItem>,
    /// Page and delta counts.
    pub counts: AttentionCounts,
    /// Stateless delta continuation.
    pub cursor: AttentionCursor,
    /// Per-host reachability; present when more than the local host was in scope.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hosts: Vec<AttentionHostStatus>,
    /// Incomplete or unreadable sources.
    pub degraded: Vec<AttentionDegraded>,
    /// Explicit human-readable result, including empty and quiet states.
    pub message: String,
}
