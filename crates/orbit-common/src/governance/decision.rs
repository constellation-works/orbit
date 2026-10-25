//! Human decision requests: listing and showing the requests held work is
//! waiting on, and the operator CLI's answer verb.
//!
//! Listing and showing are read-only tools any caller may use. Answering is
//! the operator's: its operation carries no MCP scope and the noun registers
//! tool-only operations inactive, so no agent session can answer a request
//! through the tool surface. The bare CLI applies the answer in core as the
//! human writer.
use crate::governance::operation::{
    CliArgKind, CliHelp, Effect, EnumValue, NounSpec, OperationSpec, ParamSpec, ParamType,
    ToolOnlyRegistration, UnknownFields, both, free_text, mcp_only, operation_registry, required,
};
use orbit_types::tool::McpToolScope;

operation_registry! {
    noun: DECISION;
    /// The decision-request verbs.
    pub enum DecisionVerb;
    /// Decision operations in declaration order.
    pub const DECISION_OPERATIONS;
    {
        /// Open decision requests across the workspace.
        List => LIST,
        /// One decision request with its answers and binding.
        Show => SHOW,
        /// Answer one open decision request (operator CLI only).
        Answer => ANSWER,
    }
}

/// The decision noun's shared declaration.
pub const DECISION: NounSpec<DecisionVerb> = NounSpec {
    cli_name: "decision",
    tool_prefix: "orbit.decision",
    tool_only_registration: ToolOnlyRegistration::Inactive,
    cli_about: "List, show and answer human decision requests on held work",
    audit_target_type: "decision_request",
    audit_target_param: "id",
    cli_groups: &[],
    operations: DECISION_OPERATIONS,
};

const GATES: &[EnumValue] = &[
    EnumValue::plain("block_triage"),
    EnumValue::plain("pilot_admission"),
    EnumValue::plain("proposed_approval"),
    EnumValue::plain("review_approval"),
];

const MODEL: &[ParamSpec] = &[mcp_only(
    "model",
    ParamType::String,
    "Preferred provenance field. Pass the canonical agent family (codex, claude, gemini, grok, or muse); full model strings are accepted and auto-normalized.",
)];

const LIST_FILTER: &[ParamSpec] = &[
    both(
        "gate",
        ParamType::Enum(GATES),
        "Only requests waiting on this gate: block_triage, pilot_admission, proposed_approval or review_approval.",
        CliArgKind::flag("gate"),
        "Only requests waiting on this gate",
    ),
    both(
        "task_id",
        ParamType::String,
        "Only requests on this task.",
        CliArgKind::flag("task"),
        "Only requests on this task",
    ),
    both(
        "include_closed",
        ParamType::Switch,
        "Include answered, superseded and expired requests beside the open ones.",
        CliArgKind::flag("all"),
        "Include answered, superseded and expired requests",
    ),
];

const LIST: OperationSpec<DecisionVerb> = OperationSpec {
    verb: DecisionVerb::List,
    cli_path: &["list"],
    tool_name: "orbit.decision.list",
    tool_description: "List the workspace's human decision requests on held work, open ones unless include_closed is set. Each request names its task, gate, allowed answers, expiry and effective state (open, answered, superseded or expired, with supersession and expiry derived when read). Filter by gate or task_id. Read-only: seeing a request never answers it; an operator answers from the CLI or the dashboard.",
    cli_about: "List decision requests held work is waiting on",
    cli_help: CliHelp::NONE,
    effect: Effect::Read,
    params: &[LIST_FILTER, MODEL],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
};

const REQUEST_ID: &[ParamSpec] = &[required(both(
    "id",
    ParamType::String,
    "The decision request id, as `orbit.decision.list` prints it.",
    CliArgKind::Positional,
    "The decision request id",
))];

const SHOW: OperationSpec<DecisionVerb> = OperationSpec {
    verb: DecisionVerb::Show,
    cli_path: &["show"],
    tool_name: "orbit.decision.show",
    tool_description: "Show one human decision request: its task, gate, allowed answers with their consequences, the binding it was raised against, its expiry, its effective state, and its resolution when answered. Read-only.",
    cli_about: "Show one decision request with its answers and binding",
    cli_help: CliHelp::NONE,
    effect: Effect::Read,
    params: &[REQUEST_ID, MODEL],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
};

const ANSWER_PARAMS: &[ParamSpec] = &[
    required(both(
        "answer",
        ParamType::String,
        "The chosen answer id, one of the request's allowed answers.",
        CliArgKind::Positional,
        "The chosen answer id",
    )),
    free_text(both(
        "note",
        ParamType::String,
        "Evidence or note kept with the answer. A pilot-admission verdict uses it as its evidence.",
        CliArgKind::flag("note"),
        "Evidence or note kept with the answer",
    )),
];

const ANSWER: OperationSpec<DecisionVerb> = OperationSpec {
    verb: DecisionVerb::Answer,
    cli_path: &["answer"],
    tool_name: "orbit.decision.answer",
    tool_description: "Answer one open human decision request as the operator. Operator CLI only: this operation has no MCP scope and is registered inactive as a tool, so an agent session cannot answer a request. The answer is applied as the human writer and the request then reads answered; a superseded or expired request is refused and nothing changes.",
    cli_about: "Answer one open decision request as the operator",
    cli_help: CliHelp::NONE,
    effect: Effect::Write,
    params: &[REQUEST_ID, ANSWER_PARAMS],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: None,
};
