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
    EnumValue::plain("last50"),
    EnumValue::plain("last200"),
    EnumValue::plain("last500"),
    EnumValue::plain("last1000"),
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
            "Counts work in this period ending now: 1h, 24h, 7d, 30d, 90d or all (served as 90d, the scan's reach), or over the newest delivery attempts the scan holds: last50, last200, last500 or last1000. Default: 30d.",
            CliArgKind::flag("window"),
            "Count work in this period, or over the newest delivery attempts",
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

/// The by-type cell matrix switch, implementers only.
const BY_TYPE: &[ParamSpec] = &[both(
    "by_type",
    ParamType::Switch,
    "Split implementers into (crew, model, tier, type) cells using workflow.crew_performance_types, with two-level shrinkage; implementers only.",
    CliArgKind::flag("by-type"),
    "Show implementers split by task type within each tier",
)];

/// The per-task evidence export, implementers only.
const PER_TASK: &[ParamSpec] = &[both(
    "per_task",
    ParamType::Switch,
    "Export one row per implemented task for offline analysis, with its crew, model, tier, configured type, tags, context-file path prefixes and per-term counts; implementers only, and not with by_type or shadow.",
    CliArgKind::flag("per-task"),
    "Export one row per implemented task for offline analysis",
)];

/// The shadow share report, implementers with `--by-type` only.
const SHADOW: &[ParamSpec] = &[both(
    "shadow",
    ParamType::Switch,
    "For implementers with by_type, report each crew's static, drawn and would-be share from recorded draws instead of the cell matrix. Draws without a performance record are counted separately as static.",
    CliArgKind::flag("shadow"),
    "Compare recorded draw shares with the shares the recorded factor would have given",
)];

const MODEL: &[ParamSpec] = &[mcp_only(
    "model",
    ParamType::String,
    "Preferred provenance field. Pass the canonical agent family (codex, claude, gemini, grok, or muse); full model strings are accepted and auto-normalized.",
)];

const SCORECARD: OperationSpec<AgentsVerb> = OperationSpec {
    verb: AgentsVerb::Scorecard,
    cli_path: &["scorecard"],
    tool_name: "orbit.agents.scorecard",
    tool_description: "Rank one agent role's crews and models over the window by weighted outcome rate per 100 units of work, to compare crews or models for that role. `by_type`, `shadow` and `per_task` switch implementers' output to a per-type cell matrix, a draw-share report or a per-task export; `orbit.agents.show` follows a row to its tasks. Rows below minimum support are listed last without a rank. The scoring rules, switch outputs and cost notes are in the orbit skill's references/tool-surface.md, section `Agent scorecards`.",
    cli_about: "Show one role's scorecard rows by crew and model",
    cli_help: CliHelp {
        long_about: None,
        after_help: Some(
            "The table shows each row's units, score and term counts; --json carries every rate per 100\nunits and the term definitions. Follow a row to its tasks with\n`orbit agents show --role <role> --crew <crew>`.\n\n`--by-type --shadow` (implementers only) replaces the cell matrix with each crew's static,\ndrawn and would-be share from recorded draws in the window.\n\n`--per-task` (implementers only) exports one row per implemented task for offline analysis;\nuse --json for the machine-readable export.",
        ),
        flag_order: &[],
        constraints: &[],
    },
    effect: Effect::Read,
    params: &[ROLE, SELECTION, BY_TYPE, SHADOW, PER_TASK, MODEL],
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
