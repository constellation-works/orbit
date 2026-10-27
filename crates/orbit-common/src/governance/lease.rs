//! Path leases over the owner's coordination store, shared by CLI and MCP.

use crate::governance::operation::{
    CliArgKind, CliConstraint, CliHelp, Effect, EnumValue, NounSpec, OperationSpec, ParamSpec,
    ParamType, ToolOnlyRegistration, UnknownFields, aliased, both, cli_only, mcp_only,
    operation_registry, required,
};
use orbit_types::tool::McpToolScope;

operation_registry! {
    noun: LEASE;
    /// The path-lease verbs.
    pub enum LeaseVerb;
    /// Lease operations in surface order.
    pub const LEASE_OPERATIONS;
    {
        /// Request an advisory or hard lease.
        Request => REQUEST,
        /// Renew an active lease's TTL.
        Renew => RENEW,
        /// Add selectors to a granted lease.
        Expand => EXPAND,
        /// Release a lease early.
        Release => RELEASE,
        /// Inspect a lease in any state.
        Show => SHOW,
        /// List active leases.
        List => LIST,
        /// Check path coverage and overlapping holders.
        Check => CHECK,
        /// Read durable notices after a cursor.
        Notices => NOTICES,
    }
}

/// The path-lease noun's shared declaration.
pub const LEASE: NounSpec<LeaseVerb> = NounSpec {
    cli_name: "lease",
    tool_prefix: "orbit.lease",
    tool_only_registration: ToolOnlyRegistration::Inactive,
    cli_about: "Coordinate path leases with the workspace owner",
    audit_target_type: "lease",
    audit_target_param: "lease_id",
    cli_groups: &[],
    operations: LEASE_OPERATIONS,
};

const MODES: &[EnumValue] = &[EnumValue::plain("advisory"), EnumValue::plain("hard")];
const PRIORITIES: &[EnumValue] = &[
    EnumValue::plain("low"),
    EnumValue::plain("normal"),
    EnumValue::plain("high"),
];
const ID: ParamSpec = aliased(
    required(both(
        "lease_id",
        ParamType::String,
        "Lease identifier returned by request.",
        CliArgKind::Positional,
        "Lease identifier returned by request",
    )),
    &["leaseId", "lease-id"],
);
const TTL: &[ParamSpec] = &[
    aliased(
        mcp_only(
            "ttl_seconds",
            ParamType::UnsignedInteger,
            "Lifetime in seconds, from 1 to 14400. Defaults to 1800, measured from now.",
        ),
        &["ttlSeconds", "ttl-seconds"],
    ),
    cli_only(
        "ttl",
        ParamType::String,
        CliArgKind::flag("ttl"),
        "Lifetime from now, e.g. 45m or 2h (default: 30m, maximum: 4h)",
    ),
];
const IDENTITY: &[ParamSpec] = &[
    mcp_only(
        "agent",
        ParamType::String,
        "Deprecated attribution alias; prefer model.",
    ),
    mcp_only(
        "model",
        ParamType::String,
        "Agent family for attribution: codex, claude, gemini, grok or muse.",
    ),
];

const REQUEST: OperationSpec<LeaseVerb> = OperationSpec {
    verb: LeaseVerb::Request,
    cli_path: &["request"],
    tool_name: "orbit.lease.request",
    tool_description: "Request a lease on path selectors; advisory leases never block. A hard lease overlapping task locks, reservations or hard leases waits in the lease queue (granted=false, queued=true, state queued, holding nothing) and is granted in priority-then-age order once they end; read notices for the grant. wait=false refuses it instead. Returns the lease identifier, mode, principal, priority, expiry and state, plus the blocking holders and queue position when not granted. Requires lease.enabled in the workspace.",
    cli_about: "Request an advisory or hard path lease",
    cli_help: CliHelp {
        constraints: &[CliConstraint::ConflictsWith("hard", "mode")],
        ..CliHelp::NONE
    },
    effect: Effect::Write,
    params: &[
        &[
            aliased(
                required(both(
                    "selectors",
                    ParamType::StringList,
                    "File or directory selectors to lease, such as file:src/lib.rs or dir:src.",
                    CliArgKind::Positional,
                    "File or directory selectors to lease",
                )),
                &["files"],
            ),
            required(both(
                "principal",
                ParamType::String,
                "Person or named agent holding the lease (attribution over SSH).",
                CliArgKind::flag("principal"),
                "Person or named agent holding the lease",
            )),
            both(
                "mode",
                ParamType::Enum(MODES),
                "advisory or hard; defaults to the workspace lease.default_mode policy.",
                CliArgKind::flag("mode"),
                "Lease mode (default: workspace policy)",
            ),
            cli_only(
                "hard",
                ParamType::Switch,
                CliArgKind::flag("hard"),
                "Request a hard lease",
            ),
            both(
                "priority",
                ParamType::Enum(PRIORITIES),
                "low, normal or high, bounded by the caller class's ceiling; defaults to normal or the lower ceiling.",
                CliArgKind::flag("priority"),
                "Priority within the caller class's ceiling",
            ),
            both(
                "tags",
                ParamType::StringList,
                "Optional lease tags.",
                CliArgKind::flag("tag"),
                "Lease tag (repeat for several)",
            ),
            aliased(
                both(
                    "machine_id",
                    ParamType::String,
                    "Optional caller machine identity for attribution.",
                    CliArgKind::flag("remote-caller-machine-id"),
                    "Caller machine identity for attribution",
                ),
                &["remote_caller_machine_id"],
            ),
            aliased(
                both(
                    "base_branch",
                    ParamType::String,
                    "Base branch whose landing releases the lease (defaults to the workspace base).",
                    CliArgKind::flag("base"),
                    "Base branch binding",
                ),
                &["base"],
            ),
            aliased(
                both(
                    "branch",
                    ParamType::String,
                    "Working branch or #<pull-request-number> whose landing releases the lease. The owner checks on its clock cadence; TTL is the backstop.",
                    CliArgKind::flag("branch"),
                    "Working branch or #<pull-request-number> binding",
                ),
                &["bound_branch"],
            ),
            mcp_only(
                "wait",
                ParamType::Boolean,
                "Queue an overlapping hard request (default true); false refuses it with the holders named.",
            ),
            cli_only(
                "no_wait",
                ParamType::Switch,
                CliArgKind::flag("no-wait"),
                "Refuse an overlapping hard request instead of queueing it",
            ),
        ],
        TTL,
        IDENTITY,
    ],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
};

const RENEW: OperationSpec<LeaseVerb> = OperationSpec {
    verb: LeaseVerb::Renew,
    cli_path: &["renew"],
    tool_name: "orbit.lease.renew",
    tool_description: "Renew an active path lease from now with a bounded TTL. Unknown, released and expired leases are refused. Returns the renewed lease and expiry.",
    cli_about: "Renew an active lease from now",
    cli_help: CliHelp::NONE,
    effect: Effect::Write,
    params: &[&[ID], TTL],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
};

const EXPAND: OperationSpec<LeaseVerb> = OperationSpec {
    verb: LeaseVerb::Expand,
    cli_path: &["expand"],
    tool_name: "orbit.lease.expand",
    tool_description: "Add path selectors to a granted lease. Free paths are added at once; on a hard lease, blocked ones wait in the lease queue while the lease keeps what it holds. When two leases each wait on the other's expansion, the younger, lower-priority expansion is refused with a deadlock notice. Returns the added and queued selectors, blocking holders, queue position, any deadlock and the lease.",
    cli_about: "Add selectors to a granted lease",
    cli_help: CliHelp::NONE,
    effect: Effect::Write,
    params: &[
        &[
            ID,
            aliased(
                required(both(
                    "selectors",
                    ParamType::StringList,
                    "File or directory selectors to add, such as file:src/lib.rs or dir:src.",
                    CliArgKind::Positional,
                    "File or directory selectors to add",
                )),
                &["files"],
            ),
        ],
        IDENTITY,
    ],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
};

const RELEASE: OperationSpec<LeaseVerb> = OperationSpec {
    verb: LeaseVerb::Release,
    cli_path: &["release"],
    tool_name: "orbit.lease.release",
    tool_description: "Release a path lease early. Returns released=false if the lease is unknown or already ended.",
    cli_about: "Release a path lease early",
    cli_help: CliHelp::NONE,
    effect: Effect::Write,
    params: &[&[ID], IDENTITY],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
};

const SHOW: OperationSpec<LeaseVerb> = OperationSpec {
    verb: LeaseVerb::Show,
    cli_path: &["show"],
    tool_name: "orbit.lease.show",
    tool_description: "Inspect one path lease in any state, including its mode, principal, priority, selectors and expiry. Expired leases are marked expired.",
    cli_about: "Inspect a lease in any state",
    cli_help: CliHelp::NONE,
    effect: Effect::Write,
    params: &[&[ID]],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
};

const LIST: OperationSpec<LeaseVerb> = OperationSpec {
    verb: LeaseVerb::List,
    cli_path: &["list"],
    tool_name: "orbit.lease.list",
    tool_description: "List active advisory, hard and queued path leases on the workspace owner, oldest first, with state, mode, principal, priority, held and queued selectors and expiry. Expires stale leases before listing.",
    cli_about: "List active advisory and hard leases",
    cli_help: CliHelp::NONE,
    effect: Effect::Write,
    params: &[&[]],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
};

const CHECK: OperationSpec<LeaseVerb> = OperationSpec {
    verb: LeaseVerb::Check,
    cli_path: &["check"],
    tool_name: "orbit.lease.check",
    tool_description: "Check each path selector for coverage by an optional lease, unlocked-path policy and overlapping task locks, reservations and leases. Reads owner state without expiring or releasing holders.",
    cli_about: "Check path coverage and overlapping holders",
    cli_help: CliHelp::NONE,
    effect: Effect::Read,
    params: &[&[
        aliased(
            required(both(
                "paths",
                ParamType::StringList,
                "Path selectors to check, such as file:src/lib.rs.",
                CliArgKind::Positional,
                "Path selectors to check",
            )),
            &["selectors", "files"],
        ),
        aliased(
            both(
                "lease_id",
                ParamType::String,
                "Optional caller lease to check coverage against; excluded from overlapping holders.",
                CliArgKind::flag("lease-id"),
                "Caller lease to check coverage against",
            ),
            &["leaseId", "lease-id"],
        ),
    ]],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
};

const NOTICES: OperationSpec<LeaseVerb> = OperationSpec {
    verb: LeaseVerb::Notices,
    cli_path: &["notices"],
    tool_name: "orbit.lease.notices",
    tool_description: "Read durable lease notices strictly after a workspace sequence cursor, optionally for one lease. Returns notices ordered by seq and a cursor for the next page. Reads never acknowledge or expire leases. Requires lease.enabled.",
    cli_about: "Read durable lease notices after a cursor",
    cli_help: CliHelp::NONE,
    effect: Effect::Read,
    params: &[&[
        both(
            "after",
            ParamType::UnsignedInteger,
            "Exclusive sequence cursor; defaults to zero.",
            CliArgKind::flag("after"),
            "Read after this sequence (default: 0)",
        ),
        both(
            "lease_id",
            ParamType::String,
            "Optional lease to filter.",
            CliArgKind::flag("lease-id"),
            "Read notices for this lease",
        ),
        both(
            "limit",
            ParamType::UnsignedInteger,
            "Page size from 1 to 500; defaults to 100.",
            CliArgKind::flag("limit"),
            "Page size (default: 100, maximum: 500)",
        ),
        cli_only(
            "follow",
            ParamType::Switch,
            CliArgKind::flag("follow"),
            "Follow new notices; JSON output is JSONL",
        ),
    ]],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
};
