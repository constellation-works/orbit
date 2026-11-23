use crate::governance::operation::{
    CliArgKind, CliHelp, Effect, EnumValue, NounSpec, OperationSpec, ParamType,
    ToolOnlyRegistration, UnknownFields, both, mcp_only, operation_registry,
};

/// The MCP client name the federated aggregator initializes its hops with.
/// A destination honours `candidate_read` only from a session that names it.
pub const ATTENTION_AGGREGATOR_CLIENT: &str = "orbit-federated-mux";
/// The environment variable a shell reads its attention wait grant from.
/// A flag would put the token in the process list.
pub const ATTENTION_WAIT_TOKEN_ENV: &str = "ORBIT_ATTENTION_WAIT_TOKEN";
use orbit_types::tool::{McpIdRouteDeclaration, McpToolScope};
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
    EnumValue::plain("resident_stalled"),
    EnumValue::plain("routine_tripped"),
    EnumValue::plain("drain_paused"),
    EnumValue::plain("drain_breaker_open"),
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
    tool_description: "List operator waits and suggested next actions; use at session start or on-call triage, checking degraded sources before treating an empty result as clear. Details: installed Orbit skill `references/attention-reading.md`.",
    cli_about: "List what waits on an operator",
    cli_help: CliHelp::NONE,
    effect: Effect::Read,
    params: &[&[
        both(
            "workspace",
            ParamType::StringList,
            "Workspaces; conflicts with `all`.",
            CliArgKind::flag("workspace"),
            "Registered workspaces (repeatable; conflicts with --all)",
        ),
        both(
            "all",
            ParamType::Switch,
            "All active workspaces.",
            CliArgKind::flag("all"),
            "Read every active workspace on this host and every registered host",
        ),
        both(
            "host",
            ParamType::StringList,
            "Host names or IDs.",
            CliArgKind::flag("host"),
            "Host name or machine id (repeatable); narrows which hosts are read",
        ),
        both(
            "kind",
            ParamType::EnumList(KINDS),
            "Include kinds.",
            CliArgKind::flag("kind"),
            "Include kinds (repeatable)",
        ),
        both(
            "exclude_kind",
            ParamType::EnumList(KINDS),
            "Exclude kinds.",
            CliArgKind::flag("exclude-kind"),
            "Exclude kinds (repeatable)",
        ),
        both(
            "severity",
            ParamType::Enum(SEVERITIES),
            "Min severity.",
            CliArgKind::flag("severity"),
            "Minimum severity: low, medium, high or critical",
        ),
        both(
            "status",
            ParamType::StringList,
            "Task statuses.",
            CliArgKind::flag("status"),
            "Task lifecycle states (repeatable)",
        ),
        both(
            "tag",
            ParamType::StringList,
            "Task tags.",
            CliArgKind::flag("tag"),
            "Task tags (repeatable)",
        ),
        both(
            "task",
            ParamType::StringList,
            "Task IDs.",
            CliArgKind::flag("task"),
            "Task ids (repeatable)",
        ),
        both(
            "crew",
            ParamType::StringList,
            "Assigned crews.",
            CliArgKind::flag("crew"),
            "Assigned crews (repeatable)",
        ),
        both(
            "orchestrator",
            ParamType::StringList,
            "Orchestrators.",
            CliArgKind::flag("orchestrator"),
            "Attributed orchestrators (repeatable)",
        ),
        both(
            "older_than",
            ParamType::String,
            "Min age (e.g. 90m, 36h, 3d).",
            CliArgKind::flag("older-than"),
            "Minimum age, such as 90m, 36h or 3d",
        ),
        both(
            "since",
            ParamType::String,
            "Prior response cursor.",
            CliArgKind::flag("since"),
            "Opaque cursor from a prior response; unusable tokens restart the list",
        ),
        both(
            "limit",
            ParamType::Integer,
            "Limit 1-500 (default 100).",
            CliArgKind::flag("limit"),
            "Public response limit, 1–500 (default 100)",
        ),
        mcp_only(
            "wait_grant",
            ParamType::Boolean,
            "Mint `orbit attention wait` token.",
        ),
        mcp_only(
            "candidate_read",
            ParamType::Boolean,
            "Internal aggregator flag; omit.",
        ),
    ]],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Refuse,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
    id_route: McpIdRouteDeclaration::Unspecified,
};
