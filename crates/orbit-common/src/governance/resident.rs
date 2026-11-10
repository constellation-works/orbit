//! Resident orchestrator mandates: the authority grant and the mandate reads.
//!
//! `orbit.resident.grant` is governed. Who the caller is decides what it
//! does, in core and never from input: an operator's widening applies at
//! once; anyone else's widening writes nothing to the authority and raises
//! one `resident_authority` decision request quoting the relayed words
//! verbatim, which only a human answer applies. Narrowing (removing a right,
//! dropping a workspace, shortening the deadline) is open to any caller that
//! can write the assignment task. Show and list are reads. Relay carries a
//! human's words to a resident verbatim and can never change its authority;
//! control pauses, resumes, stops, wakes, takes over or releases it, which
//! only ever narrows or restores firing under the existing authority.
use crate::governance::operation::{
    CliArgKind, CliHelp, Effect, EnumValue, NounSpec, OperationSpec, ParamSpec, ParamType,
    ToolOnlyRegistration, UnknownFields, both, free_text, mcp_only, operation_registry, required,
};
use orbit_types::tool::{McpIdRouteDeclaration, McpToolScope};

operation_registry! {
    noun: RESIDENT;
    /// The resident mandate verbs.
    pub enum ResidentVerb;
    /// Resident operations in surface order.
    pub const RESIDENT_OPERATIONS;
    {
        /// Widen or narrow a mandate's authority.
        Grant => GRANT,
        /// Relay a human's direction or answer to a resident, verbatim.
        Relay => RELAY,
        /// Pause, resume, stop, wake, take over or release a resident.
        Control => CONTROL,
        /// Append the live cycle's structured checkpoint.
        Checkpoint => CHECKPOINT,
        /// One mandate with its authority, grant history and state.
        Show => SHOW,
        /// Every mandate in the workspace.
        List => LIST,
        /// The four widening measures, per mandate and across mandates.
        Metrics => METRICS,
        /// The manager's resident state, escalations, vetoes and costly pains.
        Digest => DIGEST,
    }
}

/// The resident noun's shared declaration.
pub const RESIDENT: NounSpec<ResidentVerb> = NounSpec {
    cli_name: "resident",
    tool_prefix: "orbit.resident",
    tool_only_registration: ToolOnlyRegistration::Inactive,
    cli_about: "Grant, relay to, control and inspect resident orchestrator mandates",
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

const ATTRIBUTION: &[ParamSpec] = &[mcp_only(
    "orchestrator",
    ParamType::String,
    "Orchestrator crew for this unmanaged session. Defaults to the session's configured orchestrator. Attribution only: it is recorded on a grant request as the relaying session and never grants authority.",
)];

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
    id_route: McpIdRouteDeclaration::Unspecified,
};

const SHOW: OperationSpec<ResidentVerb> = OperationSpec {
    verb: ResidentVerb::Show,
    cli_path: &["show"],
    tool_name: "orbit.resident.show",
    tool_description: "Show one resident mandate: the mandate artifact (or why it is missing or invalid), the effective authority (rights and the workspaces both the authority and the mandate cover), the authority record with its grant and narrowing history, open grant requests, the latest parsed checkpoint and live cycle, and the mandate's state (active, handed_off, ended or invalid). Read-only.",
    cli_about: "Show a resident mandate with its authority and grant history",
    cli_help: CliHelp::NONE,
    effect: Effect::Read,
    params: &[&[TASK_ID], ATTRIBUTION],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
    id_route: McpIdRouteDeclaration::Unspecified,
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
    id_route: McpIdRouteDeclaration::Unspecified,
};

const CHECKPOINT_PARAMS: &[ParamSpec] = &[
    TASK_ID,
    required(both(
        "record",
        ParamType::Object,
        "Complete resident.checkpoint v1 JSON object (kind, schema=1, mandate, cycle, run_id, crew, recovery_of, wake, authority, actions, decisions, requests, costs, notes, next_wake, continuity, outcome, probe). At most 64 KiB, 50 actions, 20 decisions, 20 costs and 2000 note characters. Action keys are resident:<mandate>:<cycle>:<positive integer>.",
        CliArgKind::flag("record"),
        "Complete resident.checkpoint v1 JSON object",
    )),
];

const METRICS_PARAMS: &[ParamSpec] = &[
    both(
        "mandate",
        ParamType::String,
        "One mandate's assignment task. Without it, every resident mandate in the workspace is reported, with workspace totals.",
        CliArgKind::flag("mandate"),
        "Report only this mandate's assignment task",
    ),
    both(
        "since",
        ParamType::String,
        "Window start: a span such as 7d, 24h or 2w, or an RFC 3339 instant. Defaults to 30d.",
        CliArgKind::flag("since"),
        "Window: a span such as 7d, 24h or 2w, or an RFC 3339 instant (default 30d)",
    ),
    both(
        "baseline",
        ParamType::String,
        "An interactive orchestration assignment task for computing the same measures. Measures that cannot be derived from its history report not_derivable, never zero. Tasks in other workspaces are read via their owner; unreachable ones are listed in references.unreachable.",
        CliArgKind::flag("baseline"),
        "Compute the same measures for this interactive assignment task",
    ),
];

const METRICS: OperationSpec<ResidentVerb> = OperationSpec {
    verb: ResidentVerb::Metrics,
    cli_path: &["metrics"],
    tool_name: "orbit.resident.metrics",
    tool_description: "Report the resident widening measures over a window, derived read-only from task history, decision requests, checkpoints and audit rows: manual interventions (per mandate and per active mandate-day), escalation load (per mandate-day and per human-day, median answer latency, share answered with the recommended option, expired-unanswered), reversal rate (resident decisions and actions a human undid within 7 days, each counted once) and crashed-cycle recovery cost (cycles without a checkpoint, time to the next good checkpoint, recovery slot minutes, repeated actions, human interventions needed). With baseline, the same measures for an interactive assignment task; a measure that cannot be derived reports not_derivable, never zero. Read-only.",
    cli_about: "Report resident interventions, escalation load, reversals and recovery cost",
    cli_help: CliHelp::NONE,
    effect: Effect::Read,
    params: &[METRICS_PARAMS, ATTRIBUTION],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
    id_route: McpIdRouteDeclaration::Unspecified,
};

const CHECKPOINT: OperationSpec<ResidentVerb> = OperationSpec {
    verb: ResidentVerb::Checkpoint,
    cli_path: &["checkpoint"],
    tool_name: "orbit.resident.checkpoint",
    tool_description: "Append one validated resident.checkpoint v1 JSON comment to the mandate's assignment task. Requires the mandate's live, unexpired cycle and matching run_id. Malformed records, stale cycles and a second checkpoint for the same cycle are refused without writes. Returns {task_id, checkpoint}. Observed authority in the record grants no rights.",
    cli_about: "Append the live cycle's structured checkpoint",
    cli_help: CliHelp::NONE,
    effect: Effect::Write,
    params: &[CHECKPOINT_PARAMS, ATTRIBUTION],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Refuse,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
    id_route: McpIdRouteDeclaration::Unspecified,
};

const DIGEST: OperationSpec<ResidentVerb> = OperationSpec {
    verb: ResidentVerb::Digest,
    cli_path: &["digest"],
    tool_name: "orbit.resident.digest",
    tool_description: "Read the manager digest in the bound workspace: resident state, latest outcome, open requests and stalled flag; open escalations oldest first; reversible decisions in the window still awaiting veto; top evolution pains by minutes plus slot minutes with occurrence, cycle and mandate counts; evolution proposals awaiting human approval; and resident metrics totals. Read-only. Defaults to 14d and the top 5 pains.",
    cli_about: "Show resident state, escalations, vetoes, top pains and open proposals",
    cli_help: CliHelp::NONE,
    effect: Effect::Read,
    params: &[
        &[
            both(
                "since",
                ParamType::String,
                "Window start as a span (7d, 24h, 2w) or RFC 3339 instant; defaults to the configured evolution window.",
                CliArgKind::flag("since"),
                "Window start (default: configured evolution window)",
            ),
            both(
                "top",
                ParamType::Integer,
                "Maximum pains to return, 1-100; default 5.",
                CliArgKind::flag("top"),
                "Maximum pains, 1-100 (default 5)",
            ),
        ],
        ATTRIBUTION,
    ],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
    id_route: McpIdRouteDeclaration::Unspecified,
};

const RELAY_PARAMS: &[ParamSpec] = &[
    TASK_ID,
    // Not free text: the quote must arrive unchanged, so core refuses text
    // that would need redaction instead of rewriting it.
    required(both(
        "quote",
        ParamType::RawString,
        "The human's words, verbatim (1-4000 characters). They reach the resident unchanged as a resident.answer comment; text that would need redaction (a secret or home path) is refused, never rewritten.",
        CliArgKind::flag("quote"),
        "The human's words, verbatim",
    )),
    required(both(
        "source",
        ParamType::Object,
        "Where the words came from: {human, via, session?, said_at}. human names who said it, via the channel (chat, email, call), session the conversation it came from, said_at an RFC 3339 instant. No other fields.",
        CliArgKind::flag("source"),
        "Provenance JSON: {\"human\", \"via\", \"session\"?, \"said_at\"}",
    )),
    both(
        "request_id",
        ParamType::String,
        "The resident request or decision request this answers. A relayed answer is a direction: the human still answers the decision request itself.",
        CliArgKind::flag("request-id"),
        "Resident request or decision request this answers",
    ),
    both(
        "reverses",
        ParamType::String,
        "The resident decision id this direction reverses.",
        CliArgKind::flag("reverses"),
        "Resident decision id this direction reverses",
    ),
];

const RELAY: OperationSpec<ResidentVerb> = OperationSpec {
    verb: ResidentVerb::Relay,
    cli_path: &["relay"],
    tool_name: "orbit.resident.relay",
    tool_description: "Relay a human's direction or answer to a resident mandate: writes one resident.answer comment carrying `quote` verbatim with `source` provenance, records your session as relayed_by, and wakes the resident. A relay never changes authority: input naming rights, workspaces, status or other fields is refused; widen through orbit.resident.grant. Refused for an empty or over-long quote (>4000 characters) or an ended mandate. Output {task_id, comment, answer}: comment is {index, at, by} (comments carry no ids) and answer is the record as written.",
    cli_about: "Relay a human's words to a resident, verbatim",
    cli_help: CliHelp::NONE,
    effect: Effect::Write,
    params: &[RELAY_PARAMS, ATTRIBUTION],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Refuse,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
    id_route: McpIdRouteDeclaration::Unspecified,
};

const CONTROL_ACTIONS: &[EnumValue] = &[
    EnumValue::plain("pause"),
    EnumValue::plain("resume"),
    EnumValue::plain("stop"),
    EnumValue::plain("wake"),
    EnumValue::plain("takeover"),
    EnumValue::plain("release"),
];

const CONTROL_PARAMS: &[ParamSpec] = &[
    TASK_ID,
    required(both(
        "action",
        ParamType::Enum(CONTROL_ACTIONS),
        "pause (active to paused: no new cycle), resume (paused to active), stop (ends the mandate: no new cycle, then the assignment moves to review), wake (active: due on the next tick despite the heartbeat, never the cadence floor), takeover (active or paused to taken_over: no cycle fires) or release (taken_over to active).",
        CliArgKind::Positional,
        "pause, resume, stop, wake, takeover or release",
    )),
    required(free_text(both(
        "reason",
        ParamType::String,
        "Why, recorded in the assignment's history with the caller.",
        CliArgKind::flag("reason"),
        "Why, recorded in the history with the caller",
    ))),
    both(
        "status_request",
        ParamType::Switch,
        "With wake: the cycle writes a checkpoint even when it has nothing else to do.",
        CliArgKind::flag("status-request"),
        "With wake: the cycle checkpoints even when idle",
    ),
    both(
        "by_session",
        ParamType::String,
        "The session taking over or acting, recorded with the action.",
        CliArgKind::flag("by-session"),
        "Session taking over or acting",
    ),
];

const CONTROL: OperationSpec<ResidentVerb> = OperationSpec {
    verb: ResidentVerb::Control,
    cli_path: &["control"],
    tool_name: "orbit.resident.control",
    tool_description: "Control a resident mandate's firing: pause, resume, stop, wake (status_request makes the cycle checkpoint even when idle), takeover or release. Each action is a history entry with the caller and reason. Pause, stop and takeover start no new cycle; a live cycle keeps its frozen grant, checkpoints and exits at its boundary. Stop ends the mandate and moves the assignment to review once no cycle is live. Resume and release restore firing under the existing authority and never widen it. Any caller that can write the assignment may call it. Output {task_id, action, state, effective_at}.",
    cli_about: "Pause, resume, stop, wake, take over or release a resident",
    cli_help: CliHelp::NONE,
    effect: Effect::Write,
    params: &[CONTROL_PARAMS, ATTRIBUTION],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Refuse,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
    id_route: McpIdRouteDeclaration::Unspecified,
};
