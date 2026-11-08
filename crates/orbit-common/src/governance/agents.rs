//! The read-only agent scorecard operations behind `orbit agents` and the
//! `orbit.agents.*` tools: one role's ranking by crew and model, one row's
//! terms with the tasks behind each count, and the required-check failures
//! recorded by cause that the model ranking reads.
use crate::governance::operation::{
    CliArgKind, CliHelp, Effect, EnumValue, NounSpec, OperationSpec, ParamSpec, ParamType,
    ToolOnlyRegistration, UnknownFields, both, defaulted, operation_registry, required,
};
use orbit_types::tool::{McpIdRouteDeclaration, McpToolScope};

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
        /// Required-check failures on candidate heads, recorded by cause.
        CiFailures => CI_FAILURES,
    }
}

/// The agents noun's shared declaration.
pub const AGENTS: NounSpec<AgentsVerb> = NounSpec {
    cli_name: "agents",
    tool_prefix: "orbit.agents",
    tool_only_registration: ToolOnlyRegistration::Inactive,
    cli_about: "Score agents by model and recorded effort",
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

const EXECUTIONS: &[EnumValue] = &[
    EnumValue::plain("managed"),
    EnumValue::plain("all"),
    EnumValue::plain("interactive"),
];

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
    defaulted(
        both(
            "execution",
            ParamType::Enum(EXECUTIONS),
            "Which tasks implementer counts read, by how each was executed (derived from its runs): managed (a job worked its final implementation attempt), all (managed and interactive) or interactive (reached review or done in an interactive session, without a job) as a separate series. Tasks neither yet are in none. Cost and run time count managed tasks whatever this says. Other roles ignore it. Default: managed.",
            CliArgKind::flag("execution"),
            "Which implementer tasks to count: managed, all or interactive",
        ),
        "managed",
    ),
    both(
        "until",
        ParamType::String,
        "End the window at this RFC 3339 instant, or this long ago (for example 30d; units m, h, d or w). The window is then [until - window, until). A count window (last50 and the other lastN values) takes the newest that many attempts before until. Omitted, the window ends now. A future instant is refused. Reviewer provisional counts stay measured against now.",
        CliArgKind::flag("until"),
        "End the window at this instant or duration ago (for example 30d)",
    ),
];

/// The by-type cell matrix switch, implementers only.
const BY_TYPE: &[ParamSpec] = &[both(
    "by_type",
    ParamType::Switch,
    "Split implementers into (crew, model, tier, type) cells using workflow.task_types, with two-level shrinkage; implementers only.",
    CliArgKind::flag("by-type"),
    "Show implementers split by task type within each tier",
)];

/// The per-task evidence export, implementers only.
const PER_TASK: &[ParamSpec] = &[both(
    "per_task",
    ParamType::Switch,
    "Export one row per implemented task for offline analysis, with its crew, model, tier, configured type, tags, context-file path prefixes and per-term counts; implementers only, and not with by_type.",
    CliArgKind::flag("per-task"),
    "Export one row per implemented task for offline analysis",
)];

const SCORECARD: OperationSpec<AgentsVerb> = OperationSpec {
    verb: AgentsVerb::Scorecard,
    cli_path: &["scorecard"],
    tool_name: "orbit.agents.scorecard",
    tool_description: "Rank one agent role's models by recorded reasoning effort over the window by weighted outcome rate per 100 units of work, to compare model-and-effort rows for that role; each row lists the crews that contributed its units, units with no recorded effort form a separate effort-unknown row, and a null effort is the provider default. `by_type` and `per_task` switch implementers' output to a per-type cell matrix or a per-task export; `orbit.agents.show` follows a row to its tasks. Rows below minimum support are listed last without a rank. The scoring rules, switch outputs and cost notes are in the orbit skill's references/tool-surface.md, section `Agent scorecards`.",
    cli_about: "Show one role's scorecard rows by model and recorded effort",
    cli_help: CliHelp {
        long_about: None,
        after_help: Some(
            "The table shows each row's units, score and term counts; --json carries every rate per 100\nunits and the term definitions. Follow a row to its tasks with\n`orbit agents show --role <role> --crew <crew>`.\n\n`--per-task` (implementers only) exports one row per implemented task for offline analysis;\nuse --json for the machine-readable export.",
        ),
        flag_order: &[],
        constraints: &[],
    },
    effect: Effect::Read,
    params: &[ROLE, SELECTION, BY_TYPE, PER_TASK],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
    id_route: McpIdRouteDeclaration::Unspecified,
};

/// The row a detail names. The row's model travels as `row_model`. Caller
/// provenance is the session, not a tool argument.
const ROW: &[ParamSpec] = &[
    required(both(
        "crew",
        ParamType::String,
        "The row's crew; for orchestrators, the orchestrator name tasks record.",
        CliArgKind::flag("crew"),
        "The row's crew, or the orchestrator name tasks record",
    )),
    both(
        "row_effort",
        ParamType::String,
        "The row's recorded reasoning effort (low, medium, high, xhigh or max), provider_default for a null effort, or unknown for work with no recorded effort; optional when the crew and model name one row.",
        CliArgKind::Flag {
            long: "effort",
            delimiter: None,
            aliases: &[],
            visible_aliases: &[],
            value_name: Some("EFFORT"),
            multiple: false,
        },
        "The row's recorded effort; optional when the crew and model name one row",
    ),
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
    params: &[ROLE, ROW, SELECTION],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
    id_route: McpIdRouteDeclaration::Unspecified,
};

const CI_FAILURE_CLASSES: &[EnumValue] = &[
    EnumValue::plain("code"),
    EnumValue::plain("flaky"),
    EnumValue::plain("infrastructure"),
    EnumValue::plain("unknown"),
];

/// The selection over recorded CI failures.
const CI_FAILURE_SELECTION: &[ParamSpec] = &[
    defaulted(
        both(
            "window",
            ParamType::String,
            "Failures first recorded in this period ending at until: all, or a duration such as 14d (units m, h, d or w). Default: 30d.",
            CliArgKind::flag("window"),
            "Failures first recorded in this period (all, or a duration such as 14d)",
        ),
        "30d",
    ),
    both(
        "until",
        ParamType::String,
        "End the window at this RFC 3339 instant, or this long ago (for example 7d). Omitted, the window ends now. A future instant is refused.",
        CliArgKind::flag("until"),
        "End the window at this instant or duration ago (for example 7d)",
    ),
    both(
        "task",
        ParamType::String,
        "Only this task's failures.",
        CliArgKind::flag("task"),
        "Only this task's failures",
    ),
    both(
        "class",
        ParamType::Enum(CI_FAILURE_CLASSES),
        "Only failures whose current class is this: code (the head's own change), flaky (the same head passed on a rerun), infrastructure (a declared infrastructure signature, the runner or network, or a base that fails the same check) or unknown (no cause established).",
        CliArgKind::flag("class"),
        "Only failures of this class: code, flaky, infrastructure or unknown",
    ),
];

const CI_FAILURES: OperationSpec<AgentsVerb> = OperationSpec {
    verb: AgentsVerb::CiFailures,
    cli_path: &["ci-failures"],
    tool_name: "orbit.agents.ci_failures",
    tool_description: "Required-check failures on candidate heads, one record per head and check with the task, the run that produced the head, the check, its error signature and its cause class (code, flaky, infrastructure or unknown), newest first, with per-class counts. Each record lists the audited class changes later evidence made. Filter by window, task and class.",
    cli_about: "List required-check failures on candidate heads by cause",
    cli_help: CliHelp {
        long_about: None,
        after_help: Some(
            "Classes: code (the failure cites a file the head changed, or the same head failed the\nsame check again on a rerun), flaky (the same head later passed the same check),\ninfrastructure (a declared infrastructure signature, the runner or network, or a base\nthat fails the same check) and unknown. --json carries each record's evidence and\nits audited class changes.",
        ),
        flag_order: &[],
        constraints: &[],
    },
    effect: Effect::Read,
    params: &[CI_FAILURE_SELECTION],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
    id_route: McpIdRouteDeclaration::Unspecified,
};
