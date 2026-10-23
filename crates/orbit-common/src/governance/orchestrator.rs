//! The deprecated `orbit orchestrator` command and `orbit.orchestrator.*`
//! tools: aliases of the agents scorecard for the orchestrators role
//! ([`crate::governance::agents`]), kept for one release.
use crate::governance::operation::{
    CliArgKind, CliHelp, Effect, EnumValue, NounSpec, OperationSpec, ParamSpec, ParamType,
    ToolOnlyRegistration, UnknownFields, both, defaulted, mcp_only, operation_registry, required,
};
use orbit_types::tool::McpToolScope;

operation_registry! {
    noun: ORCHESTRATOR;
    /// The deprecated orchestrator scorecard verbs.
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
    cli_about: "Deprecated alias of `orbit agents` for the orchestrators role",
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
            "Counts work in this period ending now: 1h, 24h, 7d, 30d, 90d or all (served as 90d). Default: 30d.",
            CliArgKind::flag("window"),
            "Count work in this period ending now",
        ),
        "30d",
    ),
    defaulted(
        both(
            "min_support",
            ParamType::Integer,
            "Units of work a row needs before it is ranked, at least 1; rows below it are listed last without a rank. Default: 10.",
            CliArgKind::flag("min-support"),
            "Units of work a row needs before it is ranked",
        ),
        "10",
    ),
];

/// The row a detail names beside its orchestrator, as `orbit agents show`
/// takes it: `--model` on the CLI, `row_model` over MCP. `model` stays the
/// caller's provenance on every tool.
const ROW_MODEL: &[ParamSpec] = &[both(
    "row_model",
    ParamType::String,
    "The row's model (`model unknown` for tasks that recorded none); optional when the crew has one row.",
    CliArgKind::Flag {
        long: "model",
        delimiter: None,
        aliases: &[],
        visible_aliases: &[],
        value_name: Some("MODEL"),
        multiple: false,
    },
    "The row's model; optional when the crew has one row",
)];

const MODEL: &[ParamSpec] = &[mcp_only(
    "model",
    ParamType::String,
    "Preferred provenance field. Pass the canonical agent family (codex, claude, gemini, grok, or muse); full model strings are accepted and auto-normalized.",
)];

const SCORECARD: OperationSpec<OrchestratorVerb> = OperationSpec {
    verb: OrchestratorVerb::Scorecard,
    cli_path: &["scorecard"],
    tool_name: "orbit.orchestrator.scorecard",
    tool_description: "Deprecated alias of `orbit.agents.scorecard` with role orchestrators, removed in the next release: returns that payload with a `deprecation` note naming the replacement.",
    cli_about: "Deprecated: use `orbit agents scorecard --role orchestrators`",
    cli_help: CliHelp::NONE,
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
    "The orchestrator name a task's orchestrator attribution records: the row's crew.",
    CliArgKind::Positional,
    "The orchestrator name tasks record",
))];

const SHOW: OperationSpec<OrchestratorVerb> = OperationSpec {
    verb: OrchestratorVerb::Show,
    cli_path: &["show"],
    tool_name: "orbit.orchestrator.show",
    tool_description: "Deprecated alias of `orbit.agents.show` with role orchestrators and the orchestrator as crew, removed in the next release: returns that payload with a `deprecation` note naming the replacement.",
    cli_about: "Deprecated: use `orbit agents show --role orchestrators --crew <name>`",
    cli_help: CliHelp::NONE,
    effect: Effect::Read,
    params: &[NAME, ROW_MODEL, SELECTION, MODEL],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
};
