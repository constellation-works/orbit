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
use orbit_types::tool::McpToolScope;

operation_registry! {
    noun: WORKFLOW;
    /// The workflow-run verbs, joining their specs to exhaustive surface tables.
    pub enum WorkflowVerb;
    /// Operations in tool-name order.
    pub const WORKFLOW_OPERATIONS;
    {
        /// `orbit.workflow.auto`.
        Auto => AUTO,
        /// `orbit.workflow.run.list`.
        RunList => RUN_LIST,
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

const RUN_ID_PARAM: ParamSpec = required(mcp_only("id", ParamType::String, "Job run ID."));

const CLAIM_TOKEN_PARAM: ParamSpec = mcp_only(
    "claim_token",
    ParamType::String,
    "Token for this workspace's exclusive claim, required when another operator holds one. Falls back to `ORBIT_WORKSPACE_CLAIM_TOKEN`.",
);

const MODEL_PARAM: ParamSpec = mcp_only(
    "model",
    ParamType::String,
    "Preferred provenance field. Pass the canonical agent family (codex, claude, gemini, or grok); full model strings are accepted and auto-normalized.",
);

const VIEW_PARAM: ParamSpec = mcp_only(
    "view",
    ParamType::String,
    "bounded: bounded metadata, explicit workspace, pagination and display truncation. Omit for the existing full-record response.",
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
            "status, start, stop or resize",
        )),
        mcp_only(
            "for_seconds",
            ParamType::Integer,
            "Required for start: window length in seconds, 1 through 86400 (24 hours)",
        ),
        mcp_only(
            "auto_concurrency",
            ParamType::Enum(AUTO_CONCURRENCY_MODES),
            "Start only: off, shadow or enforce for this local drain. Omitted captures machine.auto_concurrency.mode. Retained on resume/restart; changes no configuration. Unsupported for pull drains",
        ),
        mcp_only(
            "concurrency",
            ParamType::Integer,
            "Worker limit, positive u32. Optional for start (omitted uses runtime default); required for resize. Status previews this ceiling; omitted uses the live local drain, then live pull drain, then default. The drain's only ceiling: delivery jobs impose none of their own",
        ),
        mcp_only(
            "task",
            ParamType::StringList,
            "Status only: one task ID or a list to explain instead of the bounded backlog",
        ),
        mcp_only(
            "limit",
            ParamType::Integer,
            "Status only: maximum tasks to explain, including an explicit selection (1-500; default 50)",
        ),
        mcp_only(
            "allow_crew",
            ParamType::StringList,
            "Status only: configured crews to evaluate; omitted uses the live drain's restriction, if any",
        ),
        mcp_only(
            "id",
            ParamType::String,
            "Resize only: auto- or pull-drain run ID. Omitted targets the workspace's one live drain; refused when none or several are live",
        ),
        mcp_only(
            "if_revision",
            ParamType::Integer,
            "Resize only: apply only if the run's worker limit is still at this revision, so a concurrent change is reported rather than overwritten",
        ),
        mcp_only(
            "reason",
            ParamType::String,
            "Resize only: note recorded with the change",
        ),
        mcp_only(
            "complete",
            ParamType::Boolean,
            "Start only: explicitly authorize automatic completion for all tasks admitted by this window; default false keeps review. Does not approve proposed tasks",
        ),
        mcp_only(
            "approve_proposed",
            ParamType::Boolean,
            "Start only: each pass pilots qualifying proposed tasks (tagged no-diff-expected, or with context_files and an assessed complexity) and approves into backlog those with no duplicate, already-landed, conflict or warning finding, including tasks filed during the window. Tasks tagged no-auto-approve are never approved. Others stay proposed; status reports approved and held counts. Default false",
        ),
        mcp_only(
            "force",
            ParamType::Boolean,
            "Stop only: also cancel each live drain and stop its in-flight work. A pull drain's claimed leaves are stopped and their tasks return to the owner's backlog with a comment; a local drain's task runs are cancelled. A leaf that cannot be confirmed stopped keeps its claim and fails the call. Default false stops admissions only",
        ),
        mcp_only(
            "claim_token",
            ParamType::String,
            "Workspace claim token when held by another operator",
        ),
        required(mcp_only(
            "workspace",
            ParamType::String,
            "Exact selector returned by workspace discovery. No implicit destination.",
        )),
        MODEL_PARAM,
    ]],
    at_least_one_of: &[],
    // The control always refused fields outside its schema, `agent` included.
    retired_fields: &[RetiredField::verbatim("agent", "unknown field 'agent'")],
    unknown_fields: UnknownFields::Refuse,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
};

const RUN_LIST: OperationSpec<WorkflowVerb> = OperationSpec {
    verb: WorkflowVerb::RunList,
    cli_path: &[],
    tool_name: "orbit.workflow.run.list",
    tool_description: "List durable workflow runs newest-first with optional bounded filters.",
    cli_about: "",
    cli_help: CliHelp::NONE,
    effect: Effect::Read,
    params: &[&[
        mcp_only(
            "limit",
            ParamType::Integer,
            "Maximum runs to return (default 25; values above 200 are rejected).",
        ),
        mcp_only("job_id", ParamType::String, "Optional job ID filter."),
        mcp_only(
            "state",
            ParamType::String,
            "Optional concrete run-state filter, or `terminal`.",
        ),
        mcp_only(
            "since",
            ParamType::String,
            "Optional RFC 3339 lower bound for run creation time.",
        ),
        VIEW_PARAM,
        mcp_only("offset", ParamType::Integer, "Bounded list offset"),
        mcp_only("status", ParamType::String, "Bounded run state filter"),
        mcp_only(
            "include_catalog",
            ParamType::Boolean,
            "Bounded view only: include a paginated no-input job catalog with last-run metadata alongside runs",
        ),
    ]],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
};

const RUN_RESUME: OperationSpec<WorkflowVerb> = OperationSpec {
    verb: WorkflowVerb::RunResume,
    cli_path: &[],
    tool_name: "orbit.workflow.run.resume",
    tool_description: "Resume a terminal resumable workflow run as a new linked run.",
    cli_about: "",
    cli_help: CliHelp::NONE,
    effect: Effect::Write,
    params: &[&[
        RUN_ID_PARAM,
        CLAIM_TOKEN_PARAM,
        mcp_only(
            "source_repair",
            ParamType::Object,
            "Explicit operator recovery of a cancelled owner-local published candidate. Inspect: {action: inspect, task_id}. Submit: {action: submit, task_id, request_key, binding, diagnosis}. Uses fresh policy and review; grants no completion or auto-fix authority.",
        ),
    ]],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
};

const RUN_SHOW: OperationSpec<WorkflowVerb> = OperationSpec {
    verb: WorkflowVerb::RunShow,
    cli_path: &[],
    tool_name: "orbit.workflow.run.show",
    tool_description: "Fetch one durable workflow run by ID. The full response puts agent invocation outcome, failure reason, and any parsed answer under top-level `agent_invocation`; with `view: bounded`, find them at `run.agent_invocation`. The answer contains summary, findings, next steps, and extra result fields; stdout blob references retain the complete provider output.",
    cli_about: "",
    cli_help: CliHelp::NONE,
    effect: Effect::Read,
    params: &[&[
        RUN_ID_PARAM,
        VIEW_PARAM,
        mcp_only(
            "log_offset",
            ParamType::Integer,
            "Bounded log record offset",
        ),
        mcp_only("limit", ParamType::Integer, "Bounded detail page size"),
    ]],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
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
            "Explicit task IDs to ship; at least one is required.",
        )),
        mcp_only(
            "mode",
            ParamType::String,
            "Optional ship mode (`pr` or `local`); defaults to workspace configuration.",
        ),
        mcp_only("base", ParamType::String, "Optional base branch override."),
        mcp_only(
            "allowed_crews",
            ParamType::StringList,
            "Optional configured crews this explicit shipment may dispatch; excluded crews are rejected before provider invocation.",
        ),
        CLAIM_TOKEN_PARAM,
        MODEL_PARAM,
    ]],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
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
