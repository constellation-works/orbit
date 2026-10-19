//! The read-only agent scorecard operations behind `orbit agents` and the
//! `orbit.agents.*` tools: one role's ranking by crew and model, and one
//! row's terms with the tasks behind each count.
use crate::governance::operation::{
    CliArgKind, CliHelp, Effect, EnumValue, NounSpec, OperationSpec, ParamSpec, ParamType,
    ToolOnlyRegistration, UnknownFields, both, defaulted, mcp_only, operation_registry, required,
};
use orbit_types::tool::McpToolScope;

operation_registry! {
    noun: AGENTS;
    /// The agent scorecard verbs.
    pub enum AgentsVerb;
    /// Agent scorecard operations in declaration order.
    pub const AGENTS_OPERATIONS;
    {
        /// One role's scorecard rows.
        Scorecard => SCORECARD,
        /// One row with the tasks behind each count.
        Show => SHOW,
    }
}

/// The agents noun's shared declaration.
pub const AGENTS: NounSpec<AgentsVerb> = NounSpec {
    cli_name: "agents",
    tool_prefix: "orbit.agents",
    tool_only_registration: ToolOnlyRegistration::Inactive,
    cli_about: "Score orchestrators, implementers, reviewers and pilots by crew and model",
    audit_target_type: "agents",
    audit_target_param: "role",
    cli_groups: &[],
    operations: AGENTS_OPERATIONS,
};

const ROLES: &[EnumValue] = &[
    EnumValue::plain("orchestrators"),
    EnumValue::plain("implementers"),
    EnumValue::plain("reviewers"),
    EnumValue::plain("pilots"),
];

const WINDOWS: &[EnumValue] = &[
    EnumValue::plain("1h"),
    EnumValue::plain("24h"),
    EnumValue::plain("7d"),
    EnumValue::plain("30d"),
    EnumValue::plain("90d"),
    EnumValue::plain("all"),
];

const ROLE: &[ParamSpec] = &[required(both(
    "role",
    ParamType::Enum(ROLES),
    "The agent role to score: orchestrators, implementers, reviewers or pilots.",
    CliArgKind::flag("role"),
    "The agent role to score",
))];

/// The selection both verbs slice the workspace scan by.
const SELECTION: &[ParamSpec] = &[
    defaulted(
        both(
            "window",
            ParamType::Enum(WINDOWS),
            "Counts work in this period ending now: 1h, 24h, 7d, 30d, 90d or all (served as 90d, the scan's reach). Default: 30d.",
            CliArgKind::flag("window"),
            "Count work in this period ending now",
        ),
        "30d",
    ),
    defaulted(
        both(
            "min_support",
            ParamType::Integer,
            "Units of the role's work a row needs before it is ranked, at least 1; rows below it are listed last without a rank. Default: 10.",
            CliArgKind::flag("min-support"),
            "Units of work a row needs before it is ranked",
        ),
        "10",
    ),
];

const MODEL: &[ParamSpec] = &[mcp_only(
    "model",
    ParamType::String,
    "Preferred provenance field. Pass the canonical agent family (codex, claude, gemini, or grok); full model strings are accepted and auto-normalized.",
)];

const SCORECARD: OperationSpec<AgentsVerb> = OperationSpec {
    verb: AgentsVerb::Scorecard,
    cli_path: &["scorecard"],
    tool_name: "orbit.agents.scorecard",
    tool_description: "Rank one agent role's crews and models over the window: each term's count per 100 units of the role's work, weighted and summed, then pulled toward the fleet average. Rows below the minimum support are listed last without a rank; a role whose terms are not computed yet answers status not_computed with its terms and no rows. Scans the workspace once per call (cache_ttl_seconds 0). The payload the dashboard's Agents view reads.",
    cli_about: "Show one role's scorecard rows by crew and model",
    cli_help: CliHelp {
        long_about: None,
        after_help: Some(
            "The table shows each row's units, score and term counts; --json carries every rate per 100\nunits and the term definitions. Follow a row to its tasks with\n`orbit agents show --role <role> --crew <crew>`.",
        ),
        flag_order: &[],
        constraints: &[],
    },
    effect: Effect::Read,
    params: &[ROLE, SELECTION, MODEL],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
};

/// The row a detail names. The row's model travels as `row_model`, since
/// `model` is the caller's attribution on every tool.
const ROW: &[ParamSpec] = &[
    required(both(
        "crew",
        ParamType::String,
        "The row's crew; for orchestrators, the orchestrator name tasks record.",
        CliArgKind::flag("crew"),
        "The row's crew, or the orchestrator name tasks record",
    )),
    both(
        "row_model",
        ParamType::String,
        "The row's model (`model unknown` for runs that recorded none); optional when the crew has one row.",
        CliArgKind::Flag {
            long: "model",
            delimiter: None,
            aliases: &[],
            visible_aliases: &[],
            value_name: Some("MODEL"),
            multiple: false,
        },
        "The row's model; optional when the crew has one row",
    ),
];

const SHOW: OperationSpec<AgentsVerb> = OperationSpec {
    verb: AgentsVerb::Show,
    cli_path: &["show"],
    tool_name: "orbit.agents.show",
    tool_description: "One row of an agent role's scorecard and, for each term, its count, rate and contribution with the task IDs behind the count and the records that make it. Use it to follow a count from `orbit.agents.scorecard` to its tasks. Scans the workspace once per call (cache_ttl_seconds 0).",
    cli_about: "Show one row's terms and the tasks behind each count",
    cli_help: CliHelp::NONE,
    effect: Effect::Read,
    params: &[ROLE, ROW, SELECTION, MODEL],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
};
