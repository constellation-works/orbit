//! Evolution is a capability-gap ledger, separate from friction repairs.
use crate::governance::operation::{
    CliArgKind, CliHelp, Effect, NounSpec, OperationSpec, ParamSpec, ParamType,
    ToolOnlyRegistration, UnknownFields, both, mcp_only, operation_registry, required,
};
use orbit_types::tool::{McpIdRouteDeclaration, McpToolScope};
operation_registry! {
    noun: EVOLUTION;
    /// Evolution ledger verbs.
    pub enum EvolutionVerb;
    /// Evolution operations in CLI order.
    pub const EVOLUTION_OPERATIONS;
    {
        /// Record a cost outside a checkpoint.
        Add => ADD,
        /// List signature aggregates.
        List => LIST,
        /// Inspect a signature and its occurrences.
        Show => SHOW,
        /// Suppress proposals for a signature (operator only).
        Decline => DECLINE,
    }
}
/// Shared evolution surface declaration.
pub const EVOLUTION: NounSpec<EvolutionVerb> = NounSpec {
    cli_name: "evolution",
    tool_prefix: "orbit.evolution",
    tool_only_registration: ToolOnlyRegistration::Inactive,
    cli_about: "Record and inspect capability gaps and their measured costs",
    audit_target_type: "evolution",
    audit_target_param: "id",
    cli_groups: &[],
    operations: EVOLUTION_OPERATIONS,
};
const MODEL: ParamSpec = mcp_only(
    "model",
    ParamType::String,
    "Agent family for provenance (codex, claude, gemini, grok or muse).",
);
const ID: ParamSpec = required(both(
    "id",
    ParamType::String,
    "Evolution record identifier.",
    CliArgKind::Positional,
    "Evolution record identifier",
));
const ADD_PARAMS: &[ParamSpec] = &[
    required(both(
        "event",
        ParamType::Object,
        "Cost event: kind (workaround, manual_step, retry, wait), what (one line), minutes, retries, slot_minutes (nonnegative integers), wished (required for workaround/manual_step), signature (optional kebab-case key, 3-64 characters).",
        CliArgKind::flag("event"),
        "Cost event as JSON",
    )),
    both(
        "mandate",
        ParamType::String,
        "Optional assignment task for this occurrence.",
        CliArgKind::flag("mandate"),
        "Assignment task for this occurrence",
    ),
    both(
        "cycle",
        ParamType::String,
        "Optional cycle identifier; requires mandate.",
        CliArgKind::flag("cycle"),
        "Cycle identifier",
    ),
    MODEL,
];
const ADD: OperationSpec<EvolutionVerb> = OperationSpec {
    verb: EvolutionVerb::Add,
    cli_path: &["add"],
    tool_name: "orbit.evolution.add",
    tool_description: "Record an interactive cost event in the bound workspace. Signature events corroborate one non-terminal evolution record; unsigned costs are retained without creating a capability gap. Returns {event, record}. Does not create friction or propose work.",
    cli_about: "Record an interactive cost event",
    cli_help: CliHelp::NONE,
    effect: Effect::Write,
    params: &[ADD_PARAMS],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Refuse,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
    id_route: McpIdRouteDeclaration::Unspecified,
};
const LIST_PARAMS: &[ParamSpec] = &[
    both(
        "state",
        ParamType::String,
        "Filter open, proposed, declined or resolved; omitted returns all states.",
        CliArgKind::flag("state"),
        "Filter open, proposed, declined or resolved",
    ),
    both(
        "since",
        ParamType::String,
        "RFC 3339 lower bound for occurrence aggregates. Records without occurrences in the window are omitted.",
        CliArgKind::flag("since"),
        "Aggregate occurrences since this RFC 3339 instant",
    ),
    MODEL,
];
const LIST: OperationSpec<EvolutionVerb> = OperationSpec {
    verb: EvolutionVerb::List,
    cli_path: &["list"],
    tool_name: "orbit.evolution.list",
    tool_description: "List evolution signatures in the bound workspace, sorted by summed minutes plus slot_minutes. Returns {count, records}; each includes occurrence_count, distinct_cycles, distinct_mandates, cycle_count, mandate_count, minutes, retries and slot_minutes derived over the requested window. Friction is excluded. Declined signatures become eligible when suppression expires or window cost doubles.",
    cli_about: "List evolution signatures and summed costs",
    cli_help: CliHelp::NONE,
    effect: Effect::Read,
    params: &[LIST_PARAMS],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Refuse,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
    id_route: McpIdRouteDeclaration::Unspecified,
};
const SHOW: OperationSpec<EvolutionVerb> = OperationSpec {
    verb: EvolutionVerb::Show,
    cli_path: &["show"],
    tool_name: "orbit.evolution.show",
    tool_description: "Show one evolution record in the bound workspace, its occurrences, proposal state, suppression window and all-time aggregates. Friction identifiers are not accepted.",
    cli_about: "Show an evolution record and its occurrences",
    cli_help: CliHelp::NONE,
    effect: Effect::Read,
    params: &[&[ID, MODEL]],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Refuse,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
    id_route: McpIdRouteDeclaration::WorkspaceLocal(
        "an evolution record id is workspace-local and carries no host prefix",
    ),
};
const DECLINE: OperationSpec<EvolutionVerb> = OperationSpec {
    verb: EvolutionVerb::Decline,
    cli_path: &["decline"],
    tool_name: "orbit.evolution.decline",
    tool_description: "Operator-only: decline an evolution signature and suppress proposals for days (default 30). Occurrences keep accumulating. Suppression ends early only when window cost at least doubles.",
    cli_about: "Suppress proposals for a signature (operator only)",
    cli_help: CliHelp::NONE,
    effect: Effect::Write,
    params: &[&[
        ID,
        both(
            "days",
            ParamType::Integer,
            "Suppression days, 1-3650; defaults to 30.",
            CliArgKind::flag("days"),
            "Suppression days (default: configured suppression, initially 30)",
        ),
        MODEL,
    ]],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Refuse,
    mcp_scope: None,
    id_route: McpIdRouteDeclaration::WorkspaceLocal(
        "an evolution record id is workspace-local and carries no host prefix",
    ),
};
