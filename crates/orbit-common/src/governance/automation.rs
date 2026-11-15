//! The automation operations behind `orbit automation` and the
//! `orbit.automation.*` tools: explaining a condition trigger over live state,
//! or a job's auto-task steps as checked at load, and the operator's audited
//! clear of a stalled condition.
use crate::governance::operation::{
    CliArgKind, CliHelp, Effect, NounSpec, OperationSpec, ParamSpec, ParamType,
    ToolOnlyRegistration, UnknownFields, both, operation_registry, required,
};
use orbit_types::tool::{McpIdRouteDeclaration, McpToolScope};

operation_registry! {
    noun: AUTOMATION;
    /// The automation verbs.
    pub enum AutomationVerb;
    /// Automation operations in declaration order.
    pub const AUTOMATION_OPERATIONS;
    {
        /// One condition definition's matches, closest misses and clause tree,
        /// or one job's auto-task steps.
        Explain => EXPLAIN,
        /// Clear a stalled condition, audited.
        ClearStall => CLEAR_STALL,
    }
}

/// The automation noun's shared declaration.
pub const AUTOMATION: NounSpec<AutomationVerb> = NounSpec {
    cli_name: "automation",
    tool_prefix: "orbit.automation",
    tool_only_registration: ToolOnlyRegistration::Inactive,
    cli_about: "Explain condition-triggered routines and auto-tasks and jobs' auto-task steps, and clear their stalls",
    audit_target_type: "automation",
    audit_target_param: "definition",
    cli_groups: &[],
    operations: AUTOMATION_OPERATIONS,
};

const EXPLAIN_PARAMS: &[ParamSpec] = &[
    required(both(
        "definition",
        ParamType::String,
        "The routine or auto-task carrying the condition: its name, or `routine:<name>` / `auto-task:<name>` when a routine and an auto-task share the name. `job:<name>` explains that job's auto-task steps instead.",
        CliArgKind::Positional,
        "Routine or auto-task name (routine:<name> or auto-task:<name> when both exist), or job:<name> for a job's auto-task steps",
    )),
    both(
        "subject",
        ParamType::String,
        "The subject whose clause tree to show: a task ID for a task-scope condition, `workspace` for a workspace-scope one. Default: the first match, else the first closest miss, else the first subject evaluated.",
        CliArgKind::flag("subject"),
        "Subject whose clause tree to show (a task ID, or workspace)",
    ),
];

const EXPLAIN: OperationSpec<AutomationVerb> = OperationSpec {
    verb: AutomationVerb::Explain,
    cli_path: &["explain"],
    tool_name: "orbit.automation.explain",
    tool_description: "Explain a routine's `trigger.condition`, an auto-task's `schedule.condition`, or `job:<name>` steps against live workspace state. Reports the scope, matches and closest misses, one subject's clause tree with operand values (unreadable values are unknown, never false), and durable tracking; job mode shows the steps checked at load. Read-only: it writes nothing, evaluates no waits and mints nothing; enabled jobs run detached and park while a step waits. Details: installed Orbit skill `references/tool-surface.md`, row `Condition explain`.",
    cli_about: "Show a condition's matches, closest misses and clause tree over live state, or a job's auto-task steps",
    cli_help: CliHelp {
        long_about: None,
        after_help: Some(
            "Explain reads the task store, the drain's eligibility view, open runs and the\ncondition's stored state, and writes nothing. A task-scope condition is evaluated over\nthe tasks its status or tag prefilter selects, at most 200 per explain. Tracking shows\narm states and holds, any stall, recent fires with their upstream chain, and `self`.\njob:<name> shows a job's auto-task steps as checked at load, with each named auto-task\nresolved; it evaluates no wait.\nExamples:\n  orbit automation explain release-prep\n  orbit automation explain routine:release-on-prep-done --subject <task-id> --json\n  orbit automation explain job:release-readiness",
        ),
        flag_order: &[],
        constraints: &[],
    },
    effect: Effect::Read,
    params: &[EXPLAIN_PARAMS],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
    id_route: McpIdRouteDeclaration::Unspecified,
};

const CLEAR_STALL_PARAMS: &[ParamSpec] = &[
    required(both(
        "definition",
        ParamType::String,
        "The stalled routine or auto-task: its name, or `routine:<name>` / `auto-task:<name>` when a routine and an auto-task share the name.",
        CliArgKind::Positional,
        "Routine or auto-task name (routine:<name> or auto-task:<name> when both exist)",
    )),
    required(both(
        "reason",
        ParamType::String,
        "Operator explanation, kept in the audit record. Required.",
        CliArgKind::flag("reason"),
        "Why the stall is cleared; kept in the audit record",
    )),
];

const CLEAR_STALL: OperationSpec<AutomationVerb> = OperationSpec {
    verb: AutomationVerb::ClearStall,
    cli_path: &["clear-stall"],
    tool_name: "orbit.automation.clear_stall",
    tool_description: "Clear a condition definition's stall (match_storm, too_many_matches, loop_suspected or oscillating) on the workspace owner, recording the operator, reason and cleared stall in the condition audit. The next clock pass evaluates the condition again; a cause that persists stalls it again.",
    cli_about: "Clear a stalled condition on the workspace owner, audited",
    cli_help: CliHelp {
        long_about: None,
        after_help: Some(
            "Operator only. The stall is cleared under the condition state's generation fence\nand the audit record is written in the same transaction. Nothing fires here: the next\nclock pass evaluates the condition, and a cause that persists stalls it again.\nExamples:\n  orbit automation clear-stall release-prep --reason \"storm was a bulk import\"",
        ),
        flag_order: &[],
        constraints: &[],
    },
    effect: Effect::Write,
    params: &[CLEAR_STALL_PARAMS],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: None,
    id_route: McpIdRouteDeclaration::Unspecified,
};
