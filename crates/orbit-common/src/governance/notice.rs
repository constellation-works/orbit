//! Fleet notices: post, close, list and show short-lived, scoped, authored
//! coordination notes (incident, maintenance, info) held by the home
//! workspace's owner.
//!
//! On MCP the noun registers in the opt-in `notice` toolset; `orbit tool
//! run`, the CLI and activities reach the tools whatever the toolset. The
//! poster class (operator, orchestrator or agent) is derived from the
//! caller's session in core, never from input, which is why post and close
//! refuse unknown fields.
use crate::governance::operation::{
    CliArgKind, CliConstraint, CliHelp, Effect, EnumValue, NounSpec, OperationInput, OperationSpec,
    ParamSpec, ParamType, ToolOnlyRegistration, UnknownFields, both, cli_only, free_text, mcp_only,
    operation_registry, required,
};
use crate::{NoticeRefusalCode, OrbitError};
use orbit_types::tool::{McpIdRouteDeclaration, McpToolScope};
use serde_json::{Value, json};

operation_registry! {
    noun: NOTICE;
    /// The fleet-notice verbs.
    pub enum NoticeVerb;
    /// Notice operations in surface order.
    pub const NOTICE_OPERATIONS;
    {
        /// Post a notice, or update or corroborate the open one holding its key.
        Post => POST,
        /// Close a notice.
        Close => CLOSE,
        /// List notices by effective state.
        List => LIST,
        /// Show one notice in any state.
        Show => SHOW,
    }
}

/// The fleet-notice noun's shared declaration.
pub const NOTICE: NounSpec<NoticeVerb> = NounSpec {
    cli_name: "notice",
    tool_prefix: "orbit.notice",
    tool_only_registration: ToolOnlyRegistration::Inactive,
    cli_about: "Post, close and list fleet notices other agents and operators see",
    audit_target_type: "notice",
    audit_target_param: "id",
    cli_groups: &[],
    operations: NOTICE_OPERATIONS,
};

const SCOPES: &[EnumValue] = &[EnumValue::plain("workspace"), EnumValue::plain("global")];
const KINDS: &[EnumValue] = &[
    EnumValue::plain("incident"),
    EnumValue::plain("maintenance"),
    EnumValue::plain("info"),
];
const STATUSES: &[EnumValue] = &[
    EnumValue::plain("open"),
    EnumValue::plain("closed"),
    EnumValue::plain("expired"),
    EnumValue::plain("all"),
];
const AUTHOR_CLASSES: &[EnumValue] = &[
    EnumValue::plain("operator"),
    EnumValue::plain("orchestrator"),
    EnumValue::plain("agent"),
];

const ATTRIBUTION: &[ParamSpec] = &[mcp_only(
    "orchestrator",
    ParamType::String,
    "Orchestrator crew for this unmanaged session. Defaults to the session's configured orchestrator. Attribution only: it never outranks the operator or a managed run's binding.",
)];

const POST_PARAMS: &[ParamSpec] = &[
    required(both(
        "kind",
        ParamType::Enum(KINDS),
        "incident (something is broken now), maintenance (planned work others should hold off or plan around) or info (anything else worth knowing).",
        CliArgKind::flag("kind"),
        "incident, maintenance or info",
    )),
    required(free_text(both(
        "title",
        ParamType::String,
        "One line, 1-120 characters, no control characters.",
        CliArgKind::flag("title"),
        "One-line title",
    ))),
    free_text(both(
        "body",
        ParamType::String,
        "Plain text, at most 4096 UTF-8 bytes by default; Markdown is stored verbatim and never rendered as HTML.",
        CliArgKind::flag("body"),
        "Plain-text body",
    )),
    cli_only(
        "body_file",
        ParamType::Path,
        CliArgKind::flag("body-file"),
        "Read the body from this file",
    ),
    both(
        "scope",
        ParamType::Enum(SCOPES),
        "workspace (the home workspace only, the default) or global (every workspace whose readers reach this owner).",
        CliArgKind::flag("scope"),
        "workspace (default) or global",
    ),
    both(
        "dedupe_key",
        ParamType::String,
        "Subject key, 1-128 characters of a-z 0-9 . _ : / -. Defaults to task:<linked_task> or <kind>:<title slug>. Posting a key an open notice holds updates or corroborates that notice instead of opening a second one.",
        CliArgKind::flag("dedupe-key"),
        "Subject key; derived from the linked task or the title when omitted",
    ),
    both(
        "linked_task",
        ParamType::String,
        "Id of the task the notice is about, usually its fix. It must be a task this workspace's owner holds, in this or another of its workspaces. When that task is done the notice closes, unless close_on_linked_done is false.",
        CliArgKind::flag("link"),
        "Task the notice is about, usually its fix",
    ),
    mcp_only(
        "close_on_linked_done",
        ParamType::Boolean,
        "Close the notice when its linked task is done. Defaults to true with linked_task.",
    ),
    cli_only(
        "keep_after_done",
        ParamType::Switch,
        CliArgKind::flag("keep-after-done"),
        "Keep the notice in force after the linked task is done",
    ),
    mcp_only(
        "evidence",
        ParamType::Array,
        "Up to 8 references as {type, value} objects; type is task, run, friction, pull_request, url or commit, and value is at most 256 characters. Stored and shown, never dereferenced.",
    ),
    cli_only(
        "evidence_refs",
        ParamType::StringList,
        CliArgKind::Flag {
            long: "evidence",
            delimiter: None,
            aliases: &[],
            visible_aliases: &[],
            value_name: Some("TYPE=VALUE"),
            multiple: false,
        },
        "Evidence reference (task, run, friction, pull_request, url or commit), repeatable",
    ),
    both(
        "hosts",
        ParamType::StringList,
        "Up to 32 machine ids a maintenance notice concerns. Refused on other kinds.",
        CliArgKind::flag("host"),
        "Machine id a maintenance notice concerns, repeatable",
    ),
    both(
        "expires_in_hours",
        ParamType::Integer,
        "Hours until the notice expires. Defaults by kind (incident 72, maintenance 12, info 168); capped by poster class, and a longer request is refused rather than clamped. Mutually exclusive with expires_at.",
        CliArgKind::flag("expires-in"),
        "Hours until the notice expires",
    ),
    both(
        "expires_at",
        ParamType::String,
        "RFC 3339 instant the notice expires, at least 5 minutes ahead. Mutually exclusive with expires_in_hours.",
        CliArgKind::flag("expires-at"),
        "RFC 3339 instant the notice expires",
    ),
];

const POST: OperationSpec<NoticeVerb> = OperationSpec {
    verb: NoticeVerb::Post,
    cli_path: &["post"],
    tool_name: "orbit.notice.post",
    tool_description: "Post a fleet notice: a short, scoped note every agent and operator in its scope should see until it stops being true (an incident, planned maintenance, or other information). Not a lease notice: lease notices are events on one path lease. The notice is held by this workspace's owner, expires (defaults by kind, capped by poster class), closes when its linked task is done, and is deduplicated by dedupe_key: posting a key an open notice already holds updates it when you may edit it, or corroborates it (bumps its report count) when you may not. The response is {notice, outcome} with outcome created, updated or corroborated; a lost reply is safe to retry. Who you post as (operator, orchestrator or agent) comes from your session, never from input; a managed agent may post only incident or info notices about its own workspace, for at most 24 hours, with a linked task or evidence. A notice informs; it grants nothing.",
    cli_about: "Post a fleet notice, or update or corroborate the open one with its key",
    cli_help: CliHelp {
        long_about: None,
        after_help: None,
        flag_order: &[],
        constraints: &[
            CliConstraint::ConflictsWith("body", "body_file"),
            CliConstraint::ConflictsWith("expires_in_hours", "expires_at"),
        ],
    },
    effect: Effect::Write,
    params: &[POST_PARAMS, ATTRIBUTION],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Refuse,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
    id_route: McpIdRouteDeclaration::Unspecified,
};

const CLOSE_PARAMS: &[ParamSpec] = &[
    both(
        "id",
        ParamType::String,
        "The notice id, as orbit.notice.list prints it. Give id or dedupe_key, not both.",
        CliArgKind::Positional,
        "The notice id",
    ),
    both(
        "dedupe_key",
        ParamType::String,
        "Close the open notice holding this key in the home workspace.",
        CliArgKind::flag("dedupe-key"),
        "Close the open notice holding this key",
    ),
    required(free_text(both(
        "reason",
        ParamType::String,
        "Why the notice no longer applies, 1-240 characters.",
        CliArgKind::flag("reason"),
        "Why the notice no longer applies",
    ))),
];

const CLOSE: OperationSpec<NoticeVerb> = OperationSpec {
    verb: NoticeVerb::Close,
    cli_path: &["close"],
    tool_name: "orbit.notice.close",
    tool_description: "Close a fleet notice by id or dedupe_key with a reason. The operator may close any notice; an orchestrator may close its own crew's notices and any orchestrator- or agent-authored notice; a managed agent may close only its own task's notices. Closing a notice that is already closed or expired returns it unchanged with already_inactive: true.",
    cli_about: "Close a fleet notice",
    cli_help: CliHelp {
        long_about: None,
        after_help: None,
        flag_order: &[],
        constraints: &[CliConstraint::ConflictsWith("id", "dedupe_key")],
    },
    effect: Effect::Write,
    params: &[CLOSE_PARAMS, ATTRIBUTION],
    at_least_one_of: &["id", "dedupe_key"],
    retired_fields: &[],
    unknown_fields: UnknownFields::Refuse,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
    id_route: McpIdRouteDeclaration::Unspecified,
};

const LIST_PARAMS: &[ParamSpec] = &[
    mcp_only(
        "all",
        ParamType::Boolean,
        "List notices from every workspace homed on this owner, not only the selected one. Without a limit, every match is returned.",
    ),
    both(
        "status",
        ParamType::Enum(STATUSES),
        "Effective state: open (the default), closed, expired or all. Closed and expired notices stay listable for the retention window.",
        CliArgKind::flag("status"),
        "open (default), closed, expired or all",
    ),
    both(
        "include_global",
        ParamType::Boolean,
        "Also return global notices homed in other workspaces on this owner (default true).",
        CliArgKind::flag("include-global"),
        "Also list global notices homed elsewhere on this owner (default true)",
    ),
    both(
        "scope",
        ParamType::Enum(SCOPES),
        "Only workspace or only global notices.",
        CliArgKind::flag("scope"),
        "Only workspace or only global notices",
    ),
    both(
        "kind",
        ParamType::EnumList(KINDS),
        "Only these kinds.",
        CliArgKind::flag("kind"),
        "Only this kind, repeatable",
    ),
    both(
        "linked_task",
        ParamType::String,
        "Only notices linked to this task.",
        CliArgKind::flag("link"),
        "Only notices linked to this task",
    ),
    both(
        "author_class",
        ParamType::Enum(AUTHOR_CLASSES),
        "Only notices posted by operator, orchestrator or agent.",
        CliArgKind::flag("author-class"),
        "Only notices posted by operator, orchestrator or agent",
    ),
    both(
        "host",
        ParamType::String,
        "Only maintenance notices whose hosts include this machine id.",
        CliArgKind::flag("host"),
        "Only maintenance notices concerning this machine id",
    ),
    both(
        "limit",
        ParamType::Integer,
        "At most this many notices, 1-200 (default 50).",
        CliArgKind::flag("limit"),
        "At most this many notices (default 50)",
    ),
];

/// The `limit` a notice list read applies when the call gives none.
pub const NOTICE_LIST_DEFAULT_LIMIT: usize = 50;
/// The largest `limit` a notice list call accepts; the smallest is 1.
pub const NOTICE_LIST_MAX_LIMIT: usize = 200;

/// The explicit `limit` of a parsed `orbit.notice.list` call, `None` when the
/// call gives none. A limit outside 1..=[`NOTICE_LIST_MAX_LIMIT`] is refused
/// as a notice `invalid_field` refusal, on every route.
pub fn notice_list_limit(input: &OperationInput) -> Result<Option<usize>, OrbitError> {
    let Some(limit) = input.optional_count("limit")? else {
        return Ok(None);
    };
    if !(1..=NOTICE_LIST_MAX_LIMIT).contains(&limit) {
        return Err(OrbitError::notice_refused(
            NoticeRefusalCode::InvalidField,
            format!("`limit` must be between 1 and {NOTICE_LIST_MAX_LIMIT}"),
        ));
    }
    Ok(Some(limit))
}

/// The explicit `limit` of a raw `orbit.notice.list` call, parsed and
/// refused exactly as [`notice_list_limit`] does after the list operation
/// parses its input. Only `limit` is read: the call's other fields are left
/// to the read that receives them.
pub fn notice_list_limit_in_call(call: &Value) -> Result<Option<usize>, OrbitError> {
    match call.get("limit") {
        None => Ok(None),
        Some(limit) => notice_list_limit(&LIST.parse_input(json!({ "limit": limit }))?),
    }
}

const LIST: OperationSpec<NoticeVerb> = OperationSpec {
    verb: NoticeVerb::List,
    cli_path: &["list"],
    tool_name: "orbit.notice.list",
    tool_description: "List fleet notices, open ones unless status says otherwise. With a workspace, that workspace's notices plus global notices homed elsewhere on its owner; with all: true, every workspace homed on this owner; without a workspace in a federated session, every reachable owner's notices merged, deduplicated by id and limited once after the merge. Each notice carries its kind, scope, title, body, author class, linked task and its status, expiry, report count and effective status (open, closed or expired, derived when read). Ordered incident, maintenance, info, newest update first. Output {notices, degraded}; degraded names any owner or home that could not be read, so a shorter list never silently hides an unreachable owner. Read-only.",
    cli_about: "List fleet notices",
    cli_help: CliHelp::NONE,
    effect: Effect::Read,
    params: &[LIST_PARAMS, ATTRIBUTION],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
    id_route: McpIdRouteDeclaration::Unspecified,
};

const SHOW_PARAMS: &[ParamSpec] = &[required(both(
    "id",
    ParamType::String,
    "The notice id, as orbit.notice.list prints it.",
    CliArgKind::Positional,
    "The notice id",
))];

const SHOW: OperationSpec<NoticeVerb> = OperationSpec {
    verb: NoticeVerb::Show,
    cli_path: &["show"],
    tool_name: "orbit.notice.show",
    tool_description: "Show one fleet notice in any effective state with its untruncated body; an operator also sees its history of posts, updates, corroborations and close. Read-only.",
    cli_about: "Show one fleet notice",
    cli_help: CliHelp::NONE,
    effect: Effect::Read,
    params: &[SHOW_PARAMS, ATTRIBUTION],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
    id_route: McpIdRouteDeclaration::WorkspaceLocal(
        "a notice id is workspace-local and carries no host prefix",
    ),
};
