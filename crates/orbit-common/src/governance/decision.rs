//! Human decision requests: listing and showing the requests held work is
//! waiting on, the operator CLI's answer verb, and the delegated-human
//! channel that submits a human's answer through `orbit.decision.submit`.
//!
//! Listing and showing are read-only tools any caller may use. Answering is
//! the operator's: its operation carries no MCP scope and the noun registers
//! tool-only operations inactive, so no agent session can answer a request
//! through the tool surface. The bare CLI applies the answer in core as the
//! human writer. Submitting is a registered channel's: `orbit.decision.submit`
//! is a governed operation whose handler verifies the channel binding and
//! the per-request token in core before applying through the same human
//! funnel; agent sessions and unregistered callers are refused. Registering,
//! listing, revoking and rotating a channel are operator CLI verbs with no
//! MCP scope, like answering.
use crate::governance::operation::{
    CliArgKind, CliGroup, CliHelp, Effect, EnumValue, NounSpec, OperationSpec, ParamSpec,
    ParamType, ToolOnlyRegistration, UnknownFields, both, free_text, mcp_only, operation_registry,
    required,
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
        /// Submit a human's answer through a registered decision channel.
        Submit => SUBMIT,
        /// Register a decision channel on this host (operator CLI only).
        ChannelRegister => CHANNEL_REGISTER,
        /// List this host's decision channels (operator CLI only).
        ChannelList => CHANNEL_LIST,
        /// Revoke a decision channel on this host (operator CLI only).
        ChannelRevoke => CHANNEL_REVOKE,
        /// Rotate a decision channel's token key (operator CLI only).
        ChannelRotateKey => CHANNEL_ROTATE_KEY,
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
    cli_groups: &[CliGroup {
        name: "channel",
        cli_about: "Register, list, revoke and rotate this host's decision channels",
        params: &[],
    }],
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

const SUBMIT_PARAMS: &[ParamSpec] = &[
    required(both(
        "channel",
        ParamType::String,
        "The registered decision channel submitting the answer, as `orbit decision channel list` shows it.",
        CliArgKind::flag("channel"),
        "The registered decision channel submitting the answer",
    )),
    required(both(
        "answer",
        ParamType::String,
        "The human's chosen answer id, one of the request's allowed answers. An empty answer is refused as unparseable.",
        CliArgKind::Positional,
        "The chosen answer id",
    )),
    required(both(
        "token",
        ParamType::String,
        "The per-request single-use token minted for this request under the channel key. Core verifies it; presenting it never reveals it back.",
        CliArgKind::flag("token"),
        "The per-request token for this request",
    )),
    free_text(both(
        "note",
        ParamType::String,
        "Evidence or note kept with the answer. A pilot-admission `evaluated` answer uses it as its evidence and is refused without one.",
        CliArgKind::flag("note"),
        "Evidence or note kept with the answer",
    )),
    both(
        "sender",
        ParamType::String,
        "The transport sender the submission presents as evidence, checked against the channel's sender allowlist when the channel has one.",
        CliArgKind::flag("sender"),
        "The transport sender presenting the answer",
    ),
    both(
        "auth_results",
        ParamType::String,
        "The raw Authentication-Results header the transport forwards as evidence. When present, core requires dkim=pass and spf=pass and refuses on dmarc=fail.",
        CliArgKind::flag("auth-results"),
        "The Authentication-Results header forwarded as evidence",
    ),
];

const SUBMIT: OperationSpec<DecisionVerb> = OperationSpec {
    verb: DecisionVerb::Submit,
    cli_path: &["submit"],
    tool_name: "orbit.decision.submit",
    tool_description: "Submit a human's answer to one open decision request through a registered decision channel. Governed: callable only by the plugin the channel is bound to (through its callback session) or by the operator testing a channel — never by an agent session. Core verifies the channel is registered and unrevoked, the request is still open against its binding, the answer is allowed, and the per-request token (single-use, expiring with the request, keyed to the channel's current key epoch). Only then is the answer applied in core as the registered human, with the channel and the token fingerprint recorded as provenance. Every refusal (channel_not_registered, channel_revoked, caller_not_allowed, sender_not_allowed, authentication_failed, token_invalid, token_consumed, request_expired, request_superseded, answer_not_allowed, request_not_found) applies nothing; a parse-level refusal does not consume the token.",
    cli_about: "Submit a human's answer through a registered decision channel",
    cli_help: CliHelp::NONE,
    effect: Effect::Write,
    params: &[REQUEST_ID, SUBMIT_PARAMS, MODEL],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
};

const CHANNEL_NAME: &[ParamSpec] = &[required(both(
    "channel",
    ParamType::String,
    "The decision channel's name on this host, as `orbit decision channel list` shows it.",
    CliArgKind::Positional,
    "The decision channel's name",
))];

const CHANNEL_REGISTER_PARAMS: &[ParamSpec] = &[
    required(both(
        "human",
        ParamType::String,
        "The human identity the channel speaks for. Applied answers are written under this label, so it must name a human, not an automation actor.",
        CliArgKind::flag("human"),
        "The human identity the channel speaks for",
    )),
    required(both(
        "plugin",
        ParamType::String,
        "The plugin namespace allowed to submit through this channel. The channel key is mirrored to this plugin's `channel_key` secret.",
        CliArgKind::flag("plugin"),
        "The plugin namespace allowed to submit through this channel",
    )),
    both(
        "allowed_senders",
        ParamType::StringList,
        "A transport sender allowed to present answers on this channel. Repeat for several; when any are registered, a submission must present one of them.",
        CliArgKind::flag("allow-sender"),
        "A sender allowed to present answers (repeatable)",
    ),
];

const CHANNEL_REGISTER: OperationSpec<DecisionVerb> = OperationSpec {
    verb: DecisionVerb::ChannelRegister,
    cli_path: &["channel", "register"],
    tool_name: "orbit.decision.channel_register",
    tool_description: "Register a decision channel on this host: its name, the human it speaks for, and the plugin allowed to submit through it. Operator CLI only: this operation has no MCP scope and is registered inactive as a tool, and registering is refused from an agent envelope and from any plugin callback, so no running code can mint itself a human. Registration generates the channel key, mirrors it to the plugin's `channel_key` secret, and starts key epoch 1.",
    cli_about: "Register a decision channel on this host",
    cli_help: CliHelp::NONE,
    effect: Effect::Write,
    params: &[CHANNEL_NAME, CHANNEL_REGISTER_PARAMS],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: None,
};

const CHANNEL_LIST: OperationSpec<DecisionVerb> = OperationSpec {
    verb: DecisionVerb::ChannelList,
    cli_path: &["channel", "list"],
    tool_name: "orbit.decision.channel_list",
    tool_description: "List this host's decision channels: name, human, bound plugin, key epoch and fingerprint, allowed senders, and whether the channel is revoked. Operator CLI only: this operation has no MCP scope and is registered inactive as a tool. Key material is never shown.",
    cli_about: "List this host's decision channels",
    cli_help: CliHelp::NONE,
    effect: Effect::Read,
    params: &[MODEL],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: None,
};

const CHANNEL_REVOKE: OperationSpec<DecisionVerb> = OperationSpec {
    verb: DecisionVerb::ChannelRevoke,
    cli_path: &["channel", "revoke"],
    tool_name: "orbit.decision.channel_revoke",
    tool_description: "Revoke a decision channel on this host, immediately: its outstanding tokens stop verifying and every submission through it is refused `channel_revoked`. Operator CLI only: this operation has no MCP scope and is registered inactive as a tool, and revoking is refused from an agent envelope and from any plugin callback.",
    cli_about: "Revoke a decision channel on this host",
    cli_help: CliHelp::NONE,
    effect: Effect::Write,
    params: &[CHANNEL_NAME],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: None,
};

const CHANNEL_ROTATE_KEY: OperationSpec<DecisionVerb> = OperationSpec {
    verb: DecisionVerb::ChannelRotateKey,
    cli_path: &["channel", "rotate-key"],
    tool_name: "orbit.decision.channel_rotate_key",
    tool_description: "Rotate a decision channel's token key: a fresh key is generated and mirrored to the bound plugin's `channel_key` secret, and the key epoch bumps, invalidating every outstanding token — a token minted under the previous epoch is refused even while its request is unexpired. Open requests wait to be re-presented under fresh tokens. Operator CLI only: this operation has no MCP scope and is registered inactive as a tool, and rotating is refused from an agent envelope and from any plugin callback.",
    cli_about: "Rotate a decision channel's token key",
    cli_help: CliHelp::NONE,
    effect: Effect::Write,
    params: &[CHANNEL_NAME],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: None,
};
