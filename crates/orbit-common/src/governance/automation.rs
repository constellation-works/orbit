//! The automation operations behind `orbit automation` and the
//! `orbit.automation.*` tools: explaining a condition trigger over live state,
//! or a job's auto-task steps as checked at load, and the operator's audited
//! clear of a stalled condition.
use crate::governance::operation::{
    CliArgKind, CliHelp, Effect, NounSpec, OperationSpec, ParamSpec, ParamType,
    ToolOnlyRegistration, UnknownFields, both, mcp_only, operation_registry, required,
};
use orbit_types::tool::McpToolScope;

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
    mcp_only(
        "model",
        ParamType::String,
        "Preferred provenance field. Pass the canonical agent family (codex, claude, gemini, grok, or muse); full model strings are accepted and auto-normalized.",
    ),
];

const EXPLAIN: OperationSpec<AutomationVerb> = OperationSpec {
    verb: AutomationVerb::Explain,
    cli_path: &["explain"],
    tool_name: "orbit.automation.explain",
    tool_description: "Explain one routine's `trigger.condition` or auto-task's `schedule.condition` over the workspace's live state, without writing anything: its scope, the store prefilter and literal cross-reads, the ownership verdict, the current matches (up to 20, each with its arm state and remaining hold) and closest misses (up to 20; subjects failing exactly one top-level clause), and for one subject the tree of top-level && and || clauses with each clause's value and the operand values it read. Values that cannot be read evaluate as unknown, never false. `tracking` is the durable state: arm states and holds, any stall, the last pass and skip, recent fires with their upstream chain, and `self`. With `job:<name>` it reports that job's auto-task steps as checked at load instead: the outer `expire_when` bound, whether admission pins a source revision, and for each step its target auto-task (resolved, missing or unreadable, with its file digest, enabled state and own condition guard), `when`, `wait_until`, `expire_when`, `reuse_when`, `require_result` and the earlier steps it reads. Such a job cannot be enabled or run yet, so nothing is evaluated, waited on or minted.",
    cli_about: "Show a condition's matches, closest misses and clause tree over live state, or a job's auto-task steps",
    cli_help: CliHelp {
        long_about: None,
        after_help: Some(
            "Explain reads the task store, the drain's eligibility view, open runs and the\ncondition's stored state, and writes nothing. A task-scope condition is evaluated over\nthe tasks its status or tag prefilter selects, at most 200 per explain. Tracking shows\narm states and holds, any stall, recent fires with their upstream chain, and `self`.\njob:<name> shows a job's auto-task steps as checked at load, with each named auto-task\nresolved; such jobs cannot run yet.\nExamples:\n  orbit automation explain release-prep\n  orbit automation explain routine:release-on-prep-done --subject <task-id> --json\n  orbit automation explain job:release-readiness",
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
};
