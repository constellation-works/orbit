//! Resident orchestrator mandates: the authority grant and the mandate reads.
//!
//! `orbit.resident.grant` is governed. Who the caller is decides what it
//! does, in core and never from input: an operator's widening applies at
//! once; anyone else's widening writes nothing to the authority and raises
//! one `resident_authority` decision request quoting the relayed words
//! verbatim, which only a human answer applies. Narrowing (removing a right,
//! dropping a workspace, shortening the deadline) is open to any caller that
//! can write the assignment task. Show and list are reads.
use crate::governance::operation::{
    CliArgKind, CliHelp, Effect, EnumValue, NounSpec, OperationSpec, ParamSpec, ParamType,
    ToolOnlyRegistration, UnknownFields, both, free_text, mcp_only, operation_registry, required,
};
use orbit_types::tool::McpToolScope;

operation_registry! {
    noun: RESIDENT;
    /// The resident mandate verbs.
    pub enum ResidentVerb;
    /// Resident operations in surface order.
    pub const RESIDENT_OPERATIONS;
    {
        /// Widen or narrow a mandate's authority.
        Grant => GRANT,
        /// One mandate with its authority, grant history and state.
        Show => SHOW,
        /// Every mandate in the workspace.
        List => LIST,
    }
}

/// The resident noun's shared declaration.
pub const RESIDENT: NounSpec<ResidentVerb> = NounSpec {
    cli_name: "resident",
    tool_prefix: "orbit.resident",
    tool_only_registration: ToolOnlyRegistration::Inactive,
    cli_about: "Grant, narrow and inspect resident orchestrator mandates",
    audit_target_type: "task",
    audit_target_param: "task_id",
    cli_groups: &[],
    operations: RESIDENT_OPERATIONS,
};

const RIGHTS: &[EnumValue] = &[
    EnumValue::plain("dispatch"),
    EnumValue::plain("promote"),
    EnumValue::plain("merge"),
    EnumValue::plain("plugin_upgrade"),
];

const ATTRIBUTION: &[ParamSpec] = &[
    mcp_only(
        "orchestrator",
        ParamType::String,
        "Orchestrator crew this unmanaged session acts for. Defaults to the session's configured orchestrator. Attribution only: it is recorded on a grant request as the relaying session and never grants authority.",
    ),
    mcp_only(
        "model",
        ParamType::String,
        "Preferred provenance field. Pass the canonical agent family (codex, claude, gemini, grok, or muse); full model strings are accepted and auto-normalized.",
    ),
];

const TASK_ID: ParamSpec = required(both(
    "task_id",
    ParamType::String,
    "The mandate's assignment task: a task tagged `resident` carrying a resident/mandate.json artifact.",
    CliArgKind::Positional,
    "The mandate's assignment task",
));

const GRANT_PARAMS: &[ParamSpec] = &[
    TASK_ID,
    both(
        "add",
        ParamType::EnumList(RIGHTS),
        "Rights to add: dispatch, promote, merge or plugin_upgrade. Widening.",
        CliArgKind::flag("add"),
        "Right to add (dispatch, promote, merge, plugin_upgrade), repeatable",
    ),
    both(
        "workspaces",
        ParamType::StringList,
        "Workspaces to add to the authority's scope; each must be one the mandate lists. Widening.",
        CliArgKind::flag("add-workspace"),
        "Workspace to add to the authority scope, repeatable",
    ),
    both(
        "remove",
        ParamType::EnumList(RIGHTS),
        "Rights to remove. Narrowing: open to any caller that can write the assignment.",
        CliArgKind::flag("remove"),
        "Right to remove, repeatable",
    ),
    both(
        "drop_workspaces",
        ParamType::StringList,
        "Workspaces to drop from the authority's scope. Narrowing.",
        CliArgKind::flag("drop-workspace"),
        "Workspace to drop from the scope, repeatable",
    ),
    both(
        "deadline",
        ParamType::String,
        "A new RFC 3339 mandate deadline, earlier than the current one (or the first one). Narrowing: it rewrites the mandate's deadline. A later deadline is an ordinary mandate edit.",
        CliArgKind::flag("deadline"),
        "Shorten the mandate deadline to this RFC 3339 instant",
    ),
    free_text(both(
        "quote",
        ParamType::String,
        "The human's words authorizing a widening, verbatim (at most 2000 characters). Required when a caller without operator authority asks to widen: the grant request carries it to the human unchanged.",
        CliArgKind::flag("quote"),
        "The human's words authorizing the widening, verbatim",
    )),
];

const GRANT: OperationSpec<ResidentVerb> = OperationSpec {
    verb: ResidentVerb::Grant,
    cli_path: &["grant"],
    tool_name: "orbit.resident.grant",
    tool_description: "Widen or narrow a resident mandate's authority: its rights (dispatch, promote, merge, plugin_upgrade) and covered workspaces. Widening (add, workspaces) is the human's: from an operator it applies at once; from any other caller it changes nothing and raises one resident_authority decision request that quotes `quote` verbatim and names your session, and the authority changes only if a human answers grant. An open request for the same addition is returned, not duplicated; any authority change supersedes it. Narrowing (remove, drop_workspaces, a shorter deadline) applies at once for any caller that can write the assignment and is recorded with that caller. A call may not widen and narrow at once. Output {outcome, authority, request?} with outcome granted, requested or narrowed.",
    cli_about: "Widen or narrow a resident mandate's authority",
    cli_help: CliHelp::NONE,
    effect: Effect::Write,
    params: &[GRANT_PARAMS, ATTRIBUTION],
    at_least_one_of: &["add", "workspaces", "remove", "drop_workspaces", "deadline"],
    retired_fields: &[],
    unknown_fields: UnknownFields::Refuse,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
};

const SHOW: OperationSpec<ResidentVerb> = OperationSpec {
    verb: ResidentVerb::Show,
    cli_path: &["show"],
    tool_name: "orbit.resident.show",
    tool_description: "Show one resident mandate: the mandate artifact (or why it is missing or invalid), the effective authority (rights and the workspaces both the authority and the mandate cover), the authority record with its grant and narrowing history, open grant requests, and the mandate's state (active, handed_off, ended or invalid). Read-only.",
    cli_about: "Show a resident mandate with its authority and grant history",
    cli_help: CliHelp::NONE,
    effect: Effect::Read,
    params: &[&[TASK_ID], ATTRIBUTION],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
};

const LIST_PARAMS: &[ParamSpec] = &[both(
    "include_ended",
    ParamType::Switch,
    "Include mandates whose assignment is done, archived or rejected.",
    CliArgKind::flag("all"),
    "Include ended mandates",
)];

const LIST: OperationSpec<ResidentVerb> = OperationSpec {
    verb: ResidentVerb::List,
    cli_path: &["list"],
    tool_name: "orbit.resident.list",
    tool_description: "List the workspace's resident mandates (tasks tagged resident): each with its state, goal, deadline, effective rights and workspaces, authority revision and open grant request count. Ended mandates only with include_ended. Read-only.",
    cli_about: "List resident mandates",
    cli_help: CliHelp::NONE,
    effect: Effect::Read,
    params: &[LIST_PARAMS, ATTRIBUTION],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
};
