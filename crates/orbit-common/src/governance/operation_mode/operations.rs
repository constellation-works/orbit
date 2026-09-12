//! The operation-mode registry — every `orbit operation` verb, declared once
//! [ORB-11332].
//!
//! Adding a verb is an [`OperationModeVerb`] variant, one spec const listed in
//! [`OPERATION_MODE_OPERATIONS`], and one handler arm in `orbit-core`. The
//! CLI, MCP, and dashboard surfaces derive their wiring from this table.
//!
//! **Every string below is shipped contract.** Tool names and parameter names
//! are the MCP wire; CLI flag spellings and help text are the argv surface.

use crate::governance::operation::{
    CliArgKind, CliBinding, CliRender, Description, OperationSpec, ParamSpec, ParamType,
    find_by_name,
};
use orbit_types::tool::McpToolScope;

/// Every verb the operation noun supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OperationModeVerb {
    /// Explain the effective policy, authority, caps, and limiting reasons.
    Explain,
    /// Enable a scoped, bounded grant.
    Enable,
    /// List recent grants.
    List,
    /// Show one grant.
    Show,
    /// Stop new admissions under a grant.
    Stop,
    /// Hard-revoke a grant.
    Revoke,
}

/// An operation-mode operation specification.
pub type OperationModeOperation = OperationSpec<OperationModeVerb>;

impl OperationModeVerb {
    /// This verb's specification.
    pub fn spec(self) -> &'static OperationModeOperation {
        match self {
            OperationModeVerb::Explain => &EXPLAIN,
            OperationModeVerb::Enable => &ENABLE,
            OperationModeVerb::List => &LIST,
            OperationModeVerb::Show => &SHOW,
            OperationModeVerb::Stop => &STOP,
            OperationModeVerb::Revoke => &REVOKE,
        }
    }

    /// The verb's short name (`enable`): its CLI subcommand and audit label.
    pub fn as_str(self) -> &'static str {
        self.spec().name
    }

    /// The verb's fully-qualified tool name (`orbit.operation.enable`).
    pub fn tool_name(self) -> &'static str {
        self.spec().tool_name
    }
}

/// The operation-mode registry. Declaration order is `--help` order.
pub const OPERATION_MODE_OPERATIONS: &[OperationModeOperation] =
    &[EXPLAIN, ENABLE, LIST, SHOW, STOP, REVOKE];

/// Look up an operation-mode operation by its short verb name.
pub fn operation_mode_operation(name: &str) -> Option<&'static OperationModeOperation> {
    find_by_name(OPERATION_MODE_OPERATIONS, name)
}

const PRESET_HELP: &str =
    "Run-layer preset override: supervised or autonomous (resets the preset-managed fields)";
const COMPLETION_HELP: &str = "Run-layer completion preference: review or done (an explicit done past the delivery cap is refused)";
const LEAF_CEILING_HELP: &str = "Run-layer ceiling on concurrently live leaf runs";
const RECOVERY_EPISODES_HELP: &str = "Run-layer recovery episodes allowed per task";
const RECOVERY_MINUTES_HELP: &str = "Run-layer recovery wall-time minutes allowed per task";
const REVIEW_POLICY_HELP: &str = "Run-layer review policy: none, before-pr (holds PR creation for a fresh reviewer), or after-landing";
const CLAIM_TOKEN_HELP: &str =
    "Token for this workspace's exclusive claim, when another operator holds one";
const GRANT_ID_HELP: &str = "Grant ID; defaults to the workspace's active grant";
const MODEL_HELP: &str = "Preferred provenance field. Pass the canonical agent family (`codex`, `claude`, `gemini`, or `grok`); full model strings are accepted and auto-normalized.";

const RUN_LAYER_PARAMS: [ParamSpec; 6] = [
    text_param("preset", PRESET_HELP),
    text_param("completion", COMPLETION_HELP),
    count_param("leaf_ceiling", "leaf-ceiling", LEAF_CEILING_HELP),
    count_param(
        "recovery_episodes",
        "recovery-episodes",
        RECOVERY_EPISODES_HELP,
    ),
    count_param(
        "recovery_minutes",
        "recovery-minutes",
        RECOVERY_MINUTES_HELP,
    ),
    text_param_long("review_policy", "review-policy", REVIEW_POLICY_HELP),
];

const EXPLAIN: OperationModeOperation = OperationModeOperation {
    verb: OperationModeVerb::Explain,
    name: "explain",
    tool_name: "orbit.operation.explain",
    tool_description: "Explain the effective operation policy: every field with its winning source, the active grant, caps, and limiting reasons",
    cli_about: "Explain the effective operation policy and authority",
    params: &[
        RUN_LAYER_PARAMS[0],
        RUN_LAYER_PARAMS[1],
        RUN_LAYER_PARAMS[2],
        RUN_LAYER_PARAMS[3],
        RUN_LAYER_PARAMS[4],
        RUN_LAYER_PARAMS[5],
    ],
    rejects_agent_field: false,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
    cli_json_flag: true,
    cli_render: CliRender::AlwaysJson,
};

const ENABLE: OperationModeOperation = OperationModeOperation {
    verb: OperationModeVerb::Enable,
    name: "enable",
    tool_name: "orbit.operation.enable",
    tool_description: "Enable a scoped operation-mode grant: a finite task set, separate prepare/promote/complete rights, and a bounded admission window. Captures the effective policy once; changing a preference later does not change the grant",
    cli_about: "Enable a scoped, bounded operation-mode grant",
    params: &[
        ParamSpec {
            name: "task_ids",
            param_type: ParamType::StringList,
            required: true,
            mcp_description: Some(Description::Static(
                "Finite task set the grant covers (proposed or backlog tasks, at most 50)",
            )),
            cli: Some(CliBinding {
                kind: CliArgKind::Flag {
                    long: "task",
                    delimiter: Some(','),
                },
                help: Description::Static(
                    "Task in the finite scope; repeat or comma-separate (at most 50)",
                ),
            }),
        },
        ParamSpec {
            name: "window",
            param_type: ParamType::String,
            required: true,
            mcp_description: Some(Description::Static(
                "Admission window from now, e.g. `30m`, `2h` (at most 24h)",
            )),
            cli: Some(CliBinding {
                kind: CliArgKind::Flag {
                    long: "for",
                    delimiter: None,
                },
                help: Description::Static(
                    "Admission window from now, e.g. `30m`, `2h` (at most 24h)",
                ),
            }),
        },
        ParamSpec {
            name: "rights",
            param_type: ParamType::StringList,
            required: true,
            mcp_description: Some(Description::Static(
                "Rights to grant: prepare, promote, and/or complete (complete needs operation.delivery_cap = done)",
            )),
            cli: Some(CliBinding {
                kind: CliArgKind::Flag {
                    long: "right",
                    delimiter: Some(','),
                },
                help: Description::Static(
                    "Right to grant: prepare, promote, or complete; repeat or comma-separate",
                ),
            }),
        },
        RUN_LAYER_PARAMS[0],
        RUN_LAYER_PARAMS[1],
        RUN_LAYER_PARAMS[2],
        RUN_LAYER_PARAMS[3],
        RUN_LAYER_PARAMS[4],
        RUN_LAYER_PARAMS[5],
        text_param_long("claim_token", "claim-token", CLAIM_TOKEN_HELP),
        mcp_model_param(),
    ],
    rejects_agent_field: false,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
    cli_json_flag: true,
    cli_render: CliRender::Record,
};

const LIST: OperationModeOperation = OperationModeOperation {
    verb: OperationModeVerb::List,
    name: "list",
    tool_name: "orbit.operation.list",
    tool_description: "List this workspace's recent operation-mode grants, newest first",
    cli_about: "List recent operation-mode grants",
    params: &[count_param(
        "limit",
        "limit",
        "Maximum number of grants to return (default 20)",
    )],
    rejects_agent_field: false,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
    cli_json_flag: true,
    cli_render: CliRender::RecordTable,
};

const SHOW: OperationModeOperation = OperationModeOperation {
    verb: OperationModeVerb::Show,
    name: "show",
    tool_name: "orbit.operation.show",
    tool_description: "Show one operation-mode grant by id",
    cli_about: "Show one operation-mode grant",
    params: &[ParamSpec {
        name: "id",
        param_type: ParamType::String,
        required: true,
        mcp_description: Some(Description::Static("Grant ID")),
        cli: Some(CliBinding {
            kind: CliArgKind::Positional,
            help: Description::Static("Grant ID, as printed by `orbit operation list`"),
        }),
    }],
    rejects_agent_field: false,
    // `list` and `explain` already carry what an agent needs; one-by-id is
    // a human follow-up on the CLI surface.
    mcp_scope: None,
    cli_json_flag: true,
    cli_render: CliRender::Record,
};

const STOP: OperationModeOperation = OperationModeOperation {
    verb: OperationModeVerb::Stop,
    name: "stop",
    tool_name: "orbit.operation.stop",
    tool_description: "Stop new admissions and promotion under a grant. Admitted work keeps its captured bounds, including completion; this is not cancellation",
    cli_about: "Stop new admissions under a grant (admitted work keeps its bounds)",
    params: &[
        text_param_long("id", "id", GRANT_ID_HELP),
        text_param("reason", "Reason recorded with the stop"),
        count_param(
            "if_revision",
            "if-revision",
            "Apply only if the grant is still at this revision",
        ),
        text_param_long("claim_token", "claim-token", CLAIM_TOKEN_HELP),
        mcp_model_param(),
    ],
    rejects_agent_field: false,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
    cli_json_flag: true,
    cli_render: CliRender::Record,
};

const REVOKE: OperationModeOperation = OperationModeOperation {
    verb: OperationModeVerb::Revoke,
    name: "revoke",
    tool_name: "orbit.operation.revoke",
    tool_description: "Hard-revoke a grant: no further privileged actions, including completion of admitted work. Records the reason and stops bound drains",
    cli_about: "Hard-revoke a grant (admitted work loses privileged actions)",
    params: &[
        text_param_long("id", "id", GRANT_ID_HELP),
        text_param("reason", "Reason recorded with the revocation"),
        count_param(
            "if_revision",
            "if-revision",
            "Apply only if the grant is still at this revision",
        ),
        text_param_long("claim_token", "claim-token", CLAIM_TOKEN_HELP),
        mcp_model_param(),
    ],
    rejects_agent_field: false,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
    cli_json_flag: true,
    cli_render: CliRender::Record,
};

/// MCP-only provenance field. CLI grant verbs record the process actor.
const fn mcp_model_param() -> ParamSpec {
    ParamSpec {
        name: "model",
        param_type: ParamType::String,
        required: false,
        mcp_description: Some(Description::Static(MODEL_HELP)),
        cli: None,
    }
}

/// A `--<name> <NAME>` string flag for a wire name without an underscore.
const fn text_param(name: &'static str, description: &'static str) -> ParamSpec {
    text_param_long(name, name, description)
}

const fn text_param_long(
    name: &'static str,
    long: &'static str,
    description: &'static str,
) -> ParamSpec {
    ParamSpec {
        name,
        param_type: ParamType::String,
        required: false,
        mcp_description: Some(Description::Static(description)),
        cli: Some(CliBinding {
            kind: CliArgKind::Flag {
                long,
                delimiter: None,
            },
            help: Description::Static(description),
        }),
    }
}

/// A `--<long> <N>` integer flag whose MCP description and CLI help agree.
const fn count_param(
    name: &'static str,
    long: &'static str,
    description: &'static str,
) -> ParamSpec {
    ParamSpec {
        name,
        param_type: ParamType::Integer,
        required: false,
        mcp_description: Some(Description::Static(description)),
        cli: Some(CliBinding {
            kind: CliArgKind::Flag {
                long,
                delimiter: None,
            },
            help: Description::Static(description),
        }),
    }
}
