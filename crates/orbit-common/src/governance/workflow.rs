//! The workflow-run operation registry.
//!
//! Names, descriptions, parameter order and registration retain the shipped
//! tool contract. These operations are tool-only: the `orbit run` and
//! `orbit job` subcommands that overlap them keep their own CLI contracts and
//! authorization, so no CLI surface is derived from here.

use crate::governance::operation::{
    CliHelp, Effect, EnumValue, NounSpec, OperationSpec, ParamSpec, ParamType, RetiredField,
    ToolOnlyRegistration, UnknownFields, mcp_only, operation_registry, required,
};
use crate::governance::tool_schema::WORKSPACE_SELECTOR_DESCRIPTION;
use orbit_types::tool::{McpIdRouteDeclaration, McpToolScope};

operation_registry! {
    noun: WORKFLOW;
    /// The workflow-run verbs, joining their specs to exhaustive surface tables.
    pub enum WorkflowVerb;
    /// Operations in tool-name order.
    pub const WORKFLOW_OPERATIONS;
    {
        /// `orbit.workflow.auto`.
        Auto => AUTO,
        /// `orbit.workflow.land`.
        Land => LAND,
        /// `orbit.workflow.run.list`.
        RunList => RUN_LIST,
        /// `orbit.workflow.run.observe`.
        RunObserve => RUN_OBSERVE,
        /// `orbit.workflow.run.resume`.
        RunResume => RUN_RESUME,
        /// `orbit.workflow.run.show`.
        RunShow => RUN_SHOW,
        /// `orbit.workflow.ship`.
        Ship => SHIP,
    }
}

/// The tool-only workflow-run noun.
pub const WORKFLOW: NounSpec<WorkflowVerb> = NounSpec {
    cli_name: "run",
    tool_prefix: "orbit.workflow",
    tool_only_registration: ToolOnlyRegistration::Inactive,
    cli_about: "",
    audit_target_type: "job_run",
    audit_target_param: "id",
    cli_groups: &[],
    operations: WORKFLOW_OPERATIONS,
};

/// The actions `orbit.workflow.auto` multiplexes. Membership and each
/// action's own settings are the handler's: status is a guarded mode, the
/// drain control owns the rest.
const AUTO_ACTIONS: &[EnumValue] = &[
    EnumValue::plain("status"),
    EnumValue::plain("start"),
    EnumValue::plain("stop"),
    EnumValue::plain("resize"),
];

const AUTO_CONCURRENCY_MODES: &[EnumValue] = &[
    EnumValue::plain("off"),
    EnumValue::plain("shadow"),
    EnumValue::plain("enforce"),
];

/// The review layers a run's `review` option may add [ORB-16572] [ORB-20099].
/// `before-landing` is recommended; `before-pr` is deprecated but honoured.
const REVIEW_LAYERS: &[EnumValue] = &[
    EnumValue::helped(
        "before-pr",
        "Deprecated but honoured: hold PR creation for a reviewer; prefer before-landing.",
    ),
    EnumValue::helped(
        "before-landing",
        "Recommended: review the open PR before it lands.",
    ),
];

const RUN_ID_PARAM: ParamSpec = required(mcp_only("id", ParamType::String, "Job run ID."));

const CLAIM_TOKEN_PARAM: ParamSpec = mcp_only(
    "claim_token",
    ParamType::String,
    "Claim token (`ORBIT_WORKSPACE_CLAIM_TOKEN` fallback).",
);

const VIEW_PARAM: ParamSpec = mcp_only(
    "view",
    ParamType::String,
    "`bounded` with explicit `workspace`.",
);

const AUTO: OperationSpec<WorkflowVerb> = OperationSpec {
    verb: WorkflowVerb::Auto,
    cli_path: &[],
    tool_name: "orbit.workflow.auto",
    tool_description: "Observe workspace auto-drain readiness, start a bounded window, stop admissions while preserving admitted workers (or, with force, stop those workers too), or resize a running drain's worker limit. Resize keeps the run ID, deadline, completion authorization and dispatched children; a lower limit only stops new admissions and cancels nothing. Requires trusted operator authority and an explicit workspace. Start is not retry-safe: reconcile readiness and runs after a lost reply before another submission.",
    cli_about: "",
    cli_help: CliHelp::NONE,
    effect: Effect::Write,
    params: &[&[
        required(mcp_only(
            "action",
            ParamType::Enum(AUTO_ACTIONS),
            "`status`, `start`, `stop`, or `resize`.",
        )),
        mcp_only(
            "for_seconds",
            ParamType::Integer,
            "Start seconds (1-86400).",
        ),
        mcp_only(
            "auto_concurrency",
            ParamType::Enum(AUTO_CONCURRENCY_MODES),
            "Start local mode (off|shadow|enforce).",
        ),
        mcp_only(
            "concurrency",
            ParamType::Integer,
            "Worker limit >=1 (required resize).",
        ),
        mcp_only("task", ParamType::StringList, "Status task IDs."),
        mcp_only(
            "limit",
            ParamType::Integer,
            "Status max 1-500 (default 50).",
        ),
        mcp_only("allow_crew", ParamType::StringList, "Status crews."),
        mcp_only("id", ParamType::String, "Resize drain ID (omit if sole)."),
        mcp_only("if_revision", ParamType::Integer, "Resize CAS rev."),
        mcp_only("reason", ParamType::String, "Resize note."),
        mcp_only(
            "complete",
            ParamType::Boolean,
            "Start auto-complete (default false).",
        ),
        mcp_only(
            "review",
            ParamType::Enum(REVIEW_LAYERS),
            "Review layer (`before-landing` preferred; `before-pr` deprecated).",
        ),
        mcp_only(
            "approve_proposed",
            ParamType::Boolean,
            "Start pilot-approve proposed (default false).",
        ),
        mcp_only(
            "force",
            ParamType::Boolean,
            "Stop cancel active work (default false).",
        ),
        mcp_only(
            "claim_token",
            ParamType::String,
            "Claim token (`ORBIT_WORKSPACE_CLAIM_TOKEN` fallback).",
        ),
        required(mcp_only(
            "workspace",
            ParamType::String,
            WORKSPACE_SELECTOR_DESCRIPTION,
        )),
    ]],
    at_least_one_of: &[],
    // The control always refused fields outside its schema, `agent` included.
    retired_fields: &[RetiredField::verbatim("agent", "unknown field 'agent'")],
    unknown_fields: UnknownFields::Refuse,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
    id_route: McpIdRouteDeclaration::Unspecified,
};

const LAND: OperationSpec<WorkflowVerb> = OperationSpec {
    verb: WorkflowVerb::Land,
    cli_path: &[],
    tool_name: "orbit.workflow.land",
    tool_description: "Land a task in review: merge its one open delivery pull request at the head whose required checks were just read, then move the task to done. The forge's branch protection decides: nothing bypasses it, and any refusal answers `task_landing_refused` with the reason and changes nothing. A pull request that already merged settles without another merge, so a call whose reply was lost may be repeated. Requires operator capability or a resident cycle holding the `merge` right.",
    cli_about: "",
    cli_help: CliHelp::NONE,
    effect: Effect::Write,
    params: &[&[required(mcp_only(
        "task_id",
        ParamType::String,
        "Review task ID.",
    ))]],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
    id_route: McpIdRouteDeclaration::Unspecified,
};

const RUN_LIST: OperationSpec<WorkflowVerb> = OperationSpec {
    verb: WorkflowVerb::RunList,
    cli_path: &[],
    tool_name: "orbit.workflow.run.list",
    tool_description: "List durable workflow runs newest-first with optional bounded filters. Requires operator capability; agent sessions are refused.",
    cli_about: "",
    cli_help: CliHelp::NONE,
    effect: Effect::Read,
    params: &[&[
        mcp_only("limit", ParamType::Integer, "Max runs (<=200, default 25)."),
        mcp_only("job_id", ParamType::String, "Job ID."),
        mcp_only("state", ParamType::String, "State or `terminal`."),
        mcp_only("since", ParamType::String, "RFC 3339 min time."),
        VIEW_PARAM,
        mcp_only("offset", ParamType::Integer, "Page offset."),
        mcp_only("status", ParamType::String, "Bounded state filter."),
        mcp_only(
            "include_catalog",
            ParamType::Boolean,
            "Include catalog (`view: bounded`).",
        ),
    ]],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
    id_route: McpIdRouteDeclaration::Unspecified,
};

const RUN_RESUME: OperationSpec<WorkflowVerb> = OperationSpec {
    verb: WorkflowVerb::RunResume,
    cli_path: &[],
    tool_name: "orbit.workflow.run.resume",
    tool_description: "Resume a terminal resumable workflow run as a new linked run, or explicitly authorize review-only same-PR source repair of an eligible owner-local publication, including successful or stopped inline publication with unmet acceptance. Requires operator capability; agent sessions are refused.",
    cli_about: "",
    cli_help: CliHelp::NONE,
    effect: Effect::Write,
    params: &[&[
        RUN_ID_PARAM,
        CLAIM_TOKEN_PARAM,
        mcp_only(
            "source_repair",
            ParamType::Object,
            "{action:inspect|submit|reconcile,task_id,request_key?,binding?,diagnosis?,reservation_digest?,unmet_acceptance?,policy_digest?}; prior inspect + plan for submit/reconcile.",
        ),
    ]],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
    id_route: McpIdRouteDeclaration::WorkspaceLocal(
        "a run id is host-local: a run executes where it was admitted",
    ),
};

const RUN_OBSERVE: OperationSpec<WorkflowVerb> = OperationSpec {
    verb: WorkflowVerb::RunObserve,
    cli_path: &[],
    tool_name: "orbit.workflow.run.observe",
    tool_description: "Read a stored workflow run without reconciliation or mutation. Requires operator capability; agent sessions are refused.",
    cli_about: "",
    cli_help: CliHelp::NONE,
    effect: Effect::Read,
    params: &[&[RUN_ID_PARAM]],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
    id_route: McpIdRouteDeclaration::WorkspaceLocal(
        "a run id is host-local: a run executes where it was admitted",
    ),
};

const RUN_SHOW: OperationSpec<WorkflowVerb> = OperationSpec {
    verb: WorkflowVerb::RunShow,
    cli_path: &[],
    tool_name: "orbit.workflow.run.show",
    tool_description: "Fetch one durable workflow run by ID. The full response puts agent invocation outcome, failure reason, and any parsed answer under top-level `agent_invocation`; with `view: bounded`, find them at `run.agent_invocation`. The answer contains summary, findings, next steps, and extra result fields; stdout blob references retain the complete provider output. Requires operator capability; agent sessions are refused.",
    cli_about: "",
    cli_help: CliHelp::NONE,
    effect: Effect::Read,
    params: &[&[
        RUN_ID_PARAM,
        VIEW_PARAM,
        mcp_only("log_offset", ParamType::Integer, "Log offset."),
        mcp_only("limit", ParamType::Integer, "Page size."),
    ]],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
    id_route: McpIdRouteDeclaration::WorkspaceLocal(
        "a run id is host-local: a run executes where it was admitted",
    ),
};

const SHIP: OperationSpec<WorkflowVerb> = OperationSpec {
    verb: WorkflowVerb::Ship,
    cli_path: &[],
    tool_name: "orbit.workflow.ship",
    tool_description: "Submit explicit tasks to the review-only ship workflow and return its durable run ID. This MCP tool does not accept completion authorization; an authorized operator on the owning host can use `orbit run ship <task-id> --complete`.",
    cli_about: "",
    cli_help: CliHelp::NONE,
    effect: Effect::Write,
    params: &[&[
        required(mcp_only(
            "task_ids",
            ParamType::StringList,
            "Task IDs (>=1).",
        )),
        mcp_only(
            "mode",
            ParamType::String,
            "`pr` or `local` (default config).",
        ),
        mcp_only("base", ParamType::String, "Base branch."),
        mcp_only("allowed_crews", ParamType::StringList, "Allowed crews."),
        mcp_only(
            "review",
            ParamType::Enum(REVIEW_LAYERS),
            "Review layer (`before-landing` preferred; `before-pr` deprecated).",
        ),
        CLAIM_TOKEN_PARAM,
    ]],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
    id_route: McpIdRouteDeclaration::Unspecified,
};

/// Refuse malformed or incompatible explicit modes before generic optional
/// field normalization can turn a supplied null into an omitted setting.
pub fn validate_auto_concurrency_input(input: &serde_json::Value) -> Result<(), crate::OrbitError> {
    let Some(mode) = input.get("auto_concurrency") else {
        return Ok(());
    };
    if input.get("action").and_then(serde_json::Value::as_str) != Some("start") {
        return Err(crate::OrbitError::InvalidInput(
            "auto_concurrency is a start setting".into(),
        ));
    }
    serde_json::from_value::<orbit_types::workflow::AutoConcurrencyMode>(mode.clone())
        .map(|_| ())
        .map_err(|_| {
            crate::OrbitError::InvalidInput(
                "auto_concurrency must be off, shadow or enforce".into(),
            )
        })
}
