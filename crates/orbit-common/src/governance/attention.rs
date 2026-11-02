//! The read-only attention operation, shared by CLI and MCP.
use crate::governance::operation::{
    CliArgKind, CliHelp, Effect, EnumValue, NounSpec, OperationSpec, ParamType,
    ToolOnlyRegistration, UnknownFields, both, operation_registry,
};
use orbit_types::tool::McpToolScope;
operation_registry! {
    noun: ATTENTION;
    /// Attention operations.
    pub enum AttentionVerb;
    /// Shared attention operation registry.
    pub const ATTENTION_OPERATIONS;
    {
        /// Derive current operator waits.
        Query => QUERY,
    }
}
/// Attention's root CLI and tool declaration.
pub const ATTENTION: NounSpec<AttentionVerb> = NounSpec {
    cli_name: "attention",
    tool_prefix: "orbit.attention",
    tool_only_registration: ToolOnlyRegistration::Inactive,
    cli_about: "List what waits on an operator",
    audit_target_type: "attention",
    audit_target_param: "",
    cli_groups: &[],
    operations: ATTENTION_OPERATIONS,
};
const KINDS: &[EnumValue] = &[
    EnumValue::plain("pilot_warning"),
    EnumValue::plain("pilot_hold"),
    EnumValue::plain("blocked"),
    EnumValue::plain("decision_open"),
    EnumValue::plain("review_waiting"),
    EnumValue::plain("run_failed"),
    EnumValue::plain("run_stuck"),
    EnumValue::plain("claim_stale"),
    EnumValue::plain("ci_red"),
    EnumValue::plain("friction_untriaged"),
    EnumValue::plain("host_unhealthy"),
    EnumValue::plain("notice_open"),
];
const SEVERITIES: &[EnumValue] = &[
    EnumValue::plain("low"),
    EnumValue::plain("medium"),
    EnumValue::plain("high"),
    EnumValue::plain("critical"),
];
const QUERY: OperationSpec<AttentionVerb> = OperationSpec {
    verb: AttentionVerb::Query,
    cli_path: &["query"],
    tool_name: "orbit.attention",
    tool_description: "List current operator waits, read-only: pilot_warning, pilot_hold, blocked, decision_open, review_waiting and notice_open. Open decisions fold into the condition they decide. Filters combine AND across fields and OR within repeated fields. Kinds select sources before reads. Returns items, counts, echoed filters, degraded sources and a stateless cursor. Pass cursor.next as since for new or materially changed items; expired, invalid or unsupported cursors restart the full list. No session hook or background activity is created. Bound MCP sessions cannot widen scope with all, host or another workspace. Runtime and cross-host sources are not yet implemented.",
    cli_about: "List what waits on an operator",
    cli_help: CliHelp::NONE,
    effect: Effect::Read,
    params: &[&[
        both(
            "workspace",
            ParamType::StringList,
            "Registered workspace names or ids; accepts one string or an array. Conflicts with all.",
            CliArgKind::flag("workspace"),
            "Registered workspaces (repeatable; conflicts with --all)",
        ),
        both(
            "all",
            ParamType::Switch,
            "Read every active workspace on this host.",
            CliArgKind::flag("all"),
            "Read every active workspace on this host",
        ),
        both(
            "host",
            ParamType::StringList,
            "Host name or machine id (repeatable).",
            CliArgKind::flag("host"),
            "Host name or machine id (repeatable)",
        ),
        both(
            "kind",
            ParamType::EnumList(KINDS),
            "Include kinds (repeatable).",
            CliArgKind::flag("kind"),
            "Include kinds (repeatable)",
        ),
        both(
            "exclude_kind",
            ParamType::EnumList(KINDS),
            "Exclude kinds (repeatable).",
            CliArgKind::flag("exclude-kind"),
            "Exclude kinds (repeatable)",
        ),
        both(
            "severity",
            ParamType::Enum(SEVERITIES),
            "Minimum severity: low, medium, high or critical.",
            CliArgKind::flag("severity"),
            "Minimum severity: low, medium, high or critical",
        ),
        both(
            "status",
            ParamType::StringList,
            "Task lifecycle states (repeatable).",
            CliArgKind::flag("status"),
            "Task lifecycle states (repeatable)",
        ),
        both(
            "tag",
            ParamType::StringList,
            "Task tags (repeatable).",
            CliArgKind::flag("tag"),
            "Task tags (repeatable)",
        ),
        both(
            "task",
            ParamType::StringList,
            "Task ids (repeatable).",
            CliArgKind::flag("task"),
            "Task ids (repeatable)",
        ),
        both(
            "crew",
            ParamType::StringList,
            "Assigned crews (repeatable).",
            CliArgKind::flag("crew"),
            "Assigned crews (repeatable)",
        ),
        both(
            "orchestrator",
            ParamType::StringList,
            "Attributed orchestrators (repeatable).",
            CliArgKind::flag("orchestrator"),
            "Attributed orchestrators (repeatable)",
        ),
        both(
            "older_than",
            ParamType::String,
            "Minimum age, such as 90m, 36h or 3d.",
            CliArgKind::flag("older-than"),
            "Minimum age, such as 90m, 36h or 3d",
        ),
        both(
            "since",
            ParamType::String,
            "Opaque cursor from a prior response; unusable tokens restart the list.",
            CliArgKind::flag("since"),
            "Opaque cursor from a prior response; unusable tokens restart the list",
        ),
        both(
            "limit",
            ParamType::Integer,
            "Public response limit, 1–500 (default 100).",
            CliArgKind::flag("limit"),
            "Public response limit, 1–500 (default 100)",
        ),
    ]],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Refuse,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
};

/// Refuse widening a workspace-bound attention session with its wire code.
pub fn attention_scope_refusal() -> crate::OrbitError {
    attention_wire_refusal(
        "scope_not_permitted",
        "A bound MCP session may read only its workspace and cannot select all or host",
    )
}

/// Refuse the aggregator-reserved input from an ordinary caller.
pub fn attention_candidate_read_refusal() -> crate::OrbitError {
    attention_wire_refusal(
        "candidate_read_not_permitted",
        "Candidate reads are reserved for the federated aggregator",
    )
}

fn attention_wire_refusal(code: &str, message: &str) -> crate::OrbitError {
    crate::OrbitError::RemoteTool {
        code: crate::RemoteToolCode::Other(code.into()),
        message: message.into(),
        payload: serde_json::json!({"code": code, "message": message}),
    }
}
