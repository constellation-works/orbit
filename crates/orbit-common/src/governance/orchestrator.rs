//! The read-only orchestrator scorecard operations: the rows the dashboard's
//! Analytics › Orchestrators view shows, and one orchestrator's tasks behind
//! each count.
use crate::governance::operation::{
    CliArgKind, CliHelp, Effect, EnumValue, NounSpec, OperationSpec, ParamSpec, ParamType,
    ToolOnlyRegistration, UnknownFields, both, defaulted, mcp_only, operation_registry, required,
};
use orbit_types::tool::McpToolScope;

operation_registry! {
    noun: ORCHESTRATOR;
    /// The orchestrator scorecard verbs.
    pub enum OrchestratorVerb;
    /// Orchestrator operations in declaration order.
    pub const ORCHESTRATOR_OPERATIONS;
    {
        /// Every orchestrator's scorecard row.
        Scorecard => SCORECARD,
        /// One orchestrator's row with the tasks behind each count.
        Show => SHOW,
    }
}

/// The orchestrator noun's shared declaration.
pub const ORCHESTRATOR: NounSpec<OrchestratorVerb> = NounSpec {
    cli_name: "orchestrator",
    tool_prefix: "orbit.orchestrator",
    tool_only_registration: ToolOnlyRegistration::Inactive,
    cli_about: "Score orchestrators by the work they file and supervise",
    audit_target_type: "orchestrator",
    audit_target_param: "orchestrator",
    cli_groups: &[],
    operations: ORCHESTRATOR_OPERATIONS,
};

const WINDOWS: &[EnumValue] = &[
    EnumValue::plain("1h"),
    EnumValue::plain("24h"),
    EnumValue::plain("7d"),
    EnumValue::plain("30d"),
    EnumValue::plain("90d"),
    EnumValue::plain("all"),
];

/// The selection both verbs slice the workspace scan by.
const SELECTION: &[ParamSpec] = &[
    defaulted(
        both(
            "window",
            ParamType::Enum(WINDOWS),
            "Counts tasks created in this period ending now: 1h, 24h, 7d, 30d, 90d or all. Default: 30d.",
            CliArgKind::flag("window"),
            "Count tasks created in this period ending now",
        ),
        "30d",
    ),
    defaulted(
        both(
            "min_support",
            ParamType::Integer,
            "Owned tasks a row needs before its rates are comparable, at least 1; rows below it are marked. Default: 10.",
            CliArgKind::flag("min-support"),
            "Owned tasks a row needs before its rates are comparable",
        ),
        "10",
    ),
];

const MODEL: &[ParamSpec] = &[mcp_only(
    "model",
    ParamType::String,
    "Preferred provenance field. Pass the canonical agent family (codex, claude, gemini, or grok); full model strings are accepted and auto-normalized.",
)];

const SCORECARD: OperationSpec<OrchestratorVerb> = OperationSpec {
    verb: OrchestratorVerb::Scorecard,
    cli_path: &["scorecard"],
    tool_name: "orbit.orchestrator.scorecard",
    tool_description: "Score each orchestrator named by tasks' orchestrator attribution over the tasks it owns that were created in the window: throughput (owned, landed, complexity-weighted landed), proactivity from the typed origin (landed and dropped orchestrator_initiative tasks, unknown origin separately), spec defects (criteria or description edits after the first run, criteria blocks and escalations, incomplete or spec_ambiguous regressions) and rework (review rejects, blocked or unmerged PRs, repeat runs, operator requeues, by cause). Each count is normalized per 100 owned tasks and per 8 active hours, with support counts, then a composite whose weights are in `definitions`. The same payload the dashboard's Orchestrators view reads.",
    cli_about: "Show every orchestrator's scorecard row",
    cli_help: CliHelp {
        long_about: None,
        after_help: Some(
            "The table shows raw counts and the composite per 100 owned tasks; --json carries every rate\nper 100 owned tasks and per 8 active hours. Follow a row to its tasks with\n`orbit orchestrator show <name>`.",
        ),
        flag_order: &[],
        constraints: &[],
    },
    effect: Effect::Read,
    params: &[SELECTION, MODEL],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
};

const NAME: &[ParamSpec] = &[required(both(
    "orchestrator",
    ParamType::String,
    "The orchestrator name a task's orchestrator attribution records.",
    CliArgKind::Positional,
    "The orchestrator name tasks record",
))];

const SHOW: OperationSpec<OrchestratorVerb> = OperationSpec {
    verb: OrchestratorVerb::Show,
    cli_path: &["show"],
    tool_name: "orbit.orchestrator.show",
    tool_description: "One orchestrator's scorecard row and, for every component, the task IDs behind its count with the history records that make it (when, what, and the attributed cause for rework). Use it to follow a count from `orbit.orchestrator.scorecard` to its tasks.",
    cli_about: "Show one orchestrator's row and the tasks behind each count",
    cli_help: CliHelp::NONE,
    effect: Effect::Read,
    params: &[NAME, SELECTION, MODEL],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
};
