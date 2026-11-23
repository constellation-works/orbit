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
        /// `orbit.workflow.refresh`.
        Refresh => REFRESH,
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
    "Exclusive claim token (fallback: `ORBIT_WORKSPACE_CLAIM_TOKEN`).",
);

const VIEW_PARAM: ParamSpec = mcp_only(
    "view",
    ParamType::String,
    "Bounded metadata view; requires `workspace` (only value: `bounded`).",
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
            "status, start, stop, or resize.",
        )),
        mcp_only(
            "for_seconds",
            ParamType::Integer,
            "start: window seconds (1-86400).",
        ),
        mcp_only(
            "auto_concurrency",
            ParamType::Enum(AUTO_CONCURRENCY_MODES),
            "start: off, shadow, or enforce (local drain only).",
        ),
        mcp_only(
            "concurrency",
            ParamType::Integer,
            "Worker limit (>=1; required for resize).",
        ),
        mcp_only(
            "task",
            ParamType::StringList,
            "status: task ID(s) to explain.",
        ),
        mcp_only(
            "limit",
            ParamType::Integer,
            "status: max tasks to explain (1-500, default 50).",
        ),
        mcp_only(
            "allow_crew",
            ParamType::StringList,
            "status: crews to evaluate.",
        ),
        mcp_only(
            "id",
            ParamType::String,
            "resize: drain run ID (omit if single live drain).",
        ),
        mcp_only(
            "if_revision",
            ParamType::Integer,
            "resize: expected limit revision for CAS.",
        ),
        mcp_only("reason", ParamType::String, "resize: change note."),
        mcp_only(
            "complete",
            ParamType::Boolean,
            "start: auto-complete admitted tasks (default false keeps review).",
        ),
        mcp_only(
            "review",
            ParamType::Enum(REVIEW_LAYERS),
            "Pre-landing review (`before-landing`, or deprecated `before-pr`); adds review only (auto: start only).",
        ),
        mcp_only(
            "approve_proposed",
            ParamType::Boolean,
            "start: pilot and approve qualifying proposed tasks (default false).",
        ),
        mcp_only(
            "force",
            ParamType::Boolean,
            "stop: also cancel live drains and stop in-flight work (default false).",
        ),
        mcp_only(
            "claim_token",
            ParamType::String,
            "Exclusive claim token (fallback: `ORBIT_WORKSPACE_CLAIM_TOKEN`).",
        ),
        required(mcp_only(
            "workspace",
            ParamType::String,
            crate::governance::tool_schema::WORKSPACE_SELECTOR_DESCRIPTION,
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
        "Task in review to land.",
    ))]],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
    id_route: McpIdRouteDeclaration::Unspecified,
};

const REFRESH: OperationSpec<WorkflowVerb> = OperationSpec {
    verb: WorkflowVerb::Refresh,
    cli_path: &[],
    tool_name: "orbit.workflow.refresh",
    tool_description: "Bring a held or review task's one open Orbit delivery pull request up to the tip of its base branch with one conflict-free merge commit and start its required checks. Never rebases, rewrites or force-pushes, makes no code edits, and lands nothing. Any refusal answers `task_refresh_refused` (naming conflicting paths on conflict) and leaves the head unchanged. A head that already includes the base tip is a no-op. Requires operator capability or a resident cycle holding the `refresh` right.",
    cli_about: "",
    cli_help: CliHelp::NONE,
    effect: Effect::Write,
    params: &[&[required(mcp_only(
        "task_id",
        ParamType::String,
        "Held or review task whose PR refreshes.",
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
        mcp_only("limit", ParamType::Integer, "Max runs (1-200, default 25)."),
        mcp_only("job_id", ParamType::String, "Job ID filter."),
        mcp_only(
            "state",
            ParamType::String,
            "Run-state filter, or `terminal`.",
        ),
        mcp_only("since", ParamType::String, "Lower RFC 3339 creation bound."),
        VIEW_PARAM,
        mcp_only("offset", ParamType::Integer, "Bounded list offset."),
        mcp_only("status", ParamType::String, "Bounded run-state filter."),
        mcp_only(
            "include_catalog",
            ParamType::Boolean,
            "With `view: bounded`, include paginated job catalog.",
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
            "Operator source-repair request: {action: inspect|submit|reconcile, task_id, ...}.",
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
        mcp_only("log_offset", ParamType::Integer, "Bounded log offset."),
        mcp_only("limit", ParamType::Integer, "Bounded detail page size."),
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
            "Task IDs to ship (>=1).",
        )),
        mcp_only(
            "mode",
            ParamType::String,
            "pr or local (default: workspace config).",
        ),
        mcp_only("base", ParamType::String, "Base branch override."),
        mcp_only(
            "allowed_crews",
            ParamType::StringList,
            "Allowed crews for this shipment.",
        ),
        mcp_only(
            "review",
            ParamType::Enum(REVIEW_LAYERS),
            "Pre-landing review (`before-landing`, or deprecated `before-pr`); adds review only (auto: start only).",
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
