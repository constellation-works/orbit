//! The task write verbs, `add` and `update`.
//!
//! Both refuse an undeclared field, as their hand-written tools did, and keep
//! their shipped refusals as [`RetiredField`]s. Their CLI and MCP shapes
//! differ the most of any task verb:
//!
//! - MCP-only parameters: guarded writes (`request_id`, `expected_revision`,
//!   `verdict`, `complete`), typed `relations` and response projections. The
//!   JSON Schema conditionals guarded writes need live in `orbit-tools`'
//!   schema-extension table.
//! - CLI-only flags with no tool field: `add`'s `--parent`, `--dependencies`,
//!   `--plan`, `--ref`, `--status` and `--source-task`, and `update`'s
//!   `--approve`, `--force`, `--discard-candidate` and `--artifact`. The CLI
//!   surface table carries them in [`TASK_CLI_OPTIONS_PARAM`], which only a
//!   process-local caller may send.
//!
//! **Every string below is shipped contract.**

use orbit_types::tool::{McpIdRouteDeclaration, McpToolScope};

use super::operations::{
    TASK_COMPLEXITY_VALUES, TASK_HOST_HELP, TASK_ORIGIN_VALUES, TASK_PRIORITY_VALUES,
    TASK_TYPE_VALUES, TaskOperation, TaskVerb,
};
use crate::governance::operation::{
    CliArgKind, CliHelp, Effect, EnumValue, ParamSpec, ParamType, RETIRED_AGENT_FIELD, Redaction,
    RetiredField, UnknownFields, both, cli_only, cli_required, defaulted, free_text, mcp_only,
    preserve_empty, required,
};

/// The hidden wire field that carries `orbit task add` and `orbit task update`
/// options no tool field expresses. The task handlers honour it only for a
/// process-local caller; an MCP session that sends it is refused.
pub const TASK_CLI_OPTIONS_PARAM: &str = "cli_options";

/// The statuses `orbit task add --status` offers.
const TASK_CREATE_STATUS_VALUES: &[EnumValue] = &[
    EnumValue::helped(
        "proposed",
        "Awaiting human approval before entering the backlog",
    ),
    EnumValue::helped("backlog", "Approved and queued for work; not yet started"),
    EnumValue::helped("someday", "Future-scoped — wanted but not yet actionable"),
];

/// The statuses `orbit task update --status` offers.
const TASK_UPDATE_STATUS_VALUES: &[EnumValue] = &[
    EnumValue::plain("proposed"),
    EnumValue::plain("backlog"),
    EnumValue::plain("someday"),
    EnumValue {
        name: "in-progress",
        help: None,
        aliases: &["in_progress"],
    },
    EnumValue::plain("review"),
    EnumValue::plain("done"),
    EnumValue::plain("blocked"),
    EnumValue::plain("archived"),
    EnumValue::plain("rejected"),
];

/// CLI `--model` on the write tools. The MCP schema does not advertise it;
/// the tool boundary still reads the key when the session asserts no model.
const WRITE_MODEL_PARAM: ParamSpec = cli_only(
    "model",
    ParamType::String,
    CliArgKind::flag("model"),
    "Explicit agent model to persist on the task artifact",
);

/// `orbit task add --model`: a create also keeps a full model string, as
/// `created_by_model`. Not an MCP parameter.
const ADD_MODEL_PARAM: ParamSpec = cli_only(
    "model",
    ParamType::String,
    CliArgKind::flag("model"),
    "Creator's agent family or full model id (e.g. claude-opus-5-5); created_by records the family and a full model id is also kept as created_by_model",
);

/// The response projection a write takes.
pub(super) const WRITE_PROJECTION_PARAMS: &[ParamSpec] = &[
    mcp_only(
        "fields",
        ParamType::StringList,
        "Response fields, keyed (even one).",
    ),
    mcp_only("field", ParamType::String, "One response field."),
];

/// The CLI-only options, carried as one object.
const CLI_OPTIONS_PARAM: ParamSpec = ParamSpec {
    name: TASK_CLI_OPTIONS_PARAM,
    aliases: &[],
    param_type: ParamType::Object,
    required: false,
    preserve_empty: false,
    default: None,
    // Its free text is the redaction table's nested-object policy.
    redact: Redaction::None,
    mcp_description: None,
    cli: None,
};

const fn flag_with_delimiter(
    long: &'static str,
    aliases: &'static [&'static str],
    value_name: Option<&'static str>,
) -> CliArgKind {
    CliArgKind::Flag {
        long,
        delimiter: Some(','),
        aliases,
        visible_aliases: &[],
        value_name,
        multiple: false,
    }
}

const fn flag_named(long: &'static str, value_name: &'static str) -> CliArgKind {
    CliArgKind::Flag {
        long,
        delimiter: None,
        aliases: &[],
        visible_aliases: &[],
        value_name: Some(value_name),
        multiple: false,
    }
}

const fn path(mut param: ParamSpec) -> ParamSpec {
    param.redact = Redaction::Path;
    param
}

/// `not_before` on `orbit.task.add`.
const NOT_BEFORE_MCP_HELP: &str = "Earliest dispatch time as an RFC 3339 instant.";

/// `--not-before` on `orbit task add|update`.
const NOT_BEFORE_CLI_HELP: &str = "Earliest dispatch start: an RFC 3339 instant (`2026-11-06T09:00:00Z`) or local `YYYY-MM-DDTHH:MM`";

const ADD_RETIRED_FIELDS: &[RetiredField] = &[
    RETIRED_AGENT_FIELD,
    RetiredField::unknown_field("plan", "set it with orbit.task.update"),
    RetiredField::unknown_field("status", "set it with orbit.task.update"),
    RetiredField::unknown_field("parent_id", "set it with orbit.task.update"),
    RetiredField::unknown_field("source_task_id", "set it with orbit.task.update"),
    RetiredField::unknown_field("external_refs", "set it with orbit.task.update"),
    RetiredField::unknown_field("context", "use `context_files`"),
    RetiredField::unknown_field("comment", "set it with orbit.task.update"),
    RetiredField::unknown_field("dependencies", "set it with orbit.task.update"),
];

pub(super) const ADD: TaskOperation = TaskOperation {
    verb: TaskVerb::Add,
    cli_path: &["add"],
    tool_name: "orbit.task.add",
    tool_description: "Create a task and return its JSON; rules are in the installed Orbit skill `references/task-fields.md`.",
    cli_about: "Create a new task",
    cli_help: CliHelp {
        constraints: &[],
        long_about: None,
        after_help: Some(
            "Status, grant, selector and scheduling rules for these flags: installed Orbit skill `references/task-fields.md`.",
        ),
        flag_order: &[
            "parent_id",
            "title",
            "description",
            "acceptance_criteria",
            "dependencies",
            "tags",
            "required_tools",
            "granted_tools",
            "requires_labels",
            "not_before",
            "plan",
            "external_refs",
            "context_files",
            "allow_missing_context",
            "priority",
            "complexity",
            "type",
            "status",
            "source_task",
            "crew",
            "orchestrator",
            "orchestrator_model",
            "origin",
            "model",
        ],
    },
    effect: Effect::Write,
    params: &[
        &[
            free_text(required(both(
                "title",
                ParamType::String,
                "Task title.",
                CliArgKind::flag("title"),
                "Task title",
            ))),
            // Not wire-required: the operator command defaults it to empty, so
            // the MCP tool's `description` requirement lives in the schema
            // extension and `shape_input`. Complexity separately requires its
            // CLI flag without changing its conditional wire requirement.
            free_text(defaulted(
                both(
                    "description",
                    ParamType::String,
                    "Task description markdown.",
                    CliArgKind::flag("description"),
                    "Task description",
                ),
                "",
            )),
            // The binding key for the task store; an MCP session's own
            // workspace may supply it, and process cwd never does.
            mcp_only(
                "workspace",
                ParamType::String,
                "Workspace selector from orbit_workspace_list.",
            ),
            free_text(both(
                "acceptance_criteria",
                ParamType::StringList,
                "Acceptance criteria.",
                CliArgKind::flag("acceptance-criteria"),
                "Acceptance criteria. Repeat the flag for multiple criteria",
            )),
            both(
                "tags",
                ParamType::StringList,
                "Task tags.",
                flag_with_delimiter("tag", &[], None),
                "Task tags. Repeat or comma-separate for multiple tags. `os:linux`, `os:macos` or `os:windows` limits which host OS may run the task; any other `os:` value is rejected",
            ),
            both(
                "required_tools",
                ParamType::StringList,
                "Canonical tool names added to the activity baseline.",
                flag_with_delimiter("required-tools", &["required-tool"], None),
                "Exact canonical tool names the task adds to its agent activity baseline",
            ),
            both(
                "granted_tools",
                ParamType::StringList,
                "Operator-granted tool names.",
                flag_with_delimiter("granted-tools", &["granted-tool"], None),
                "Exact canonical tool names an operator grants the implementing activity despite its deny list (operator only)",
            ),
            both(
                "requires_labels",
                ParamType::StringList,
                "Host capability labels the task requires.",
                flag_with_delimiter("requires-labels", &["requires-label"], Some("LABEL")),
                "Capability labels a host must carry, all of them, to run the task (its `machine.labels` plus its OS). Repeat or comma-separate for multiple labels",
            ),
            both(
                "not_before",
                ParamType::String,
                NOT_BEFORE_MCP_HELP,
                flag_named("not-before", "TIME"),
                NOT_BEFORE_CLI_HELP,
            ),
            path(both(
                "context_files",
                ParamType::StringList,
                "Context selectors this task will change.",
                flag_with_delimiter("context", &[], Some("CONTEXT")),
                "Task context selectors (`file:`, `dir:`, `symbol:path#name:kind`); repeat or comma-separate",
            )),
            both(
                "allow_missing_context",
                ParamType::Switch,
                "Accept selectors for created files.",
                CliArgKind::flag("allow-missing-context"),
                "Accept context selectors for files this task will create",
            ),
            defaulted(
                both(
                    "priority",
                    ParamType::Enum(TASK_PRIORITY_VALUES),
                    "low|medium|high|critical",
                    CliArgKind::flag("priority"),
                    "Priority level",
                ),
                "medium",
            ),
            cli_required(both(
                "complexity",
                ParamType::Enum(TASK_COMPLEXITY_VALUES),
                "low|medium|hard|xhard",
                flag_named("complexity", "low|medium|hard|xhard"),
                "Task complexity (low, medium, hard, or xhard). Required",
            )),
            both(
                "type",
                ParamType::Enum(TASK_TYPE_VALUES),
                "Task type.",
                flag_named("type", "TASK_TYPE"),
                "Task type (defaults to chore)",
            ),
            mcp_only("relations", ParamType::Array, "Typed task relations."),
            both(
                "crew",
                ParamType::String,
                "Crew that runs this task.",
                CliArgKind::flag("crew"),
                "Named crew to use when running this task (default: drawn from the complexity pool, else the configured default crew)",
            ),
            both(
                "orchestrator",
                ParamType::String,
                "Crew credited for orchestration.",
                CliArgKind::flag("orchestrator"),
                "Named crew responsible for orchestration attribution",
            ),
            both(
                "orchestrator_model",
                ParamType::String,
                "Orchestrator model when it differs from the crew.",
                CliArgKind::flag("orchestrator-model"),
                "Orchestrator's model when it differs from the orchestrator crew's configured model (default: that crew's model, recorded at creation)",
            ),
            both(
                "origin",
                ParamType::Enum(TASK_ORIGIN_VALUES),
                "Provenance category, not a free-text reason: operator_request, orchestrator_initiative or automation. Omit it when unsure; put the reason in description.",
                CliArgKind::flag("origin"),
                "Why the task was filed: operator_request (a human asked), orchestrator_initiative (the orchestrator decided) or automation. Omitted, it stays unset",
            ),
        ],
        WRITE_PROJECTION_PARAMS,
        &[
            mcp_only(
                "request_id",
                ParamType::String,
                "Retry-safe creation identity.",
            ),
            ADD_MODEL_PARAM,
            cli_only(
                "parent_id",
                ParamType::String,
                CliArgKind::flag("parent"),
                "Parent task ID for hierarchical decomposition",
            ),
            cli_only(
                "dependencies",
                ParamType::StringList,
                flag_with_delimiter("dependencies", &["dependency"], None),
                "Dependency task IDs. Repeat or comma-separate for multiple dependencies",
            ),
            defaulted(
                cli_only(
                    "plan",
                    ParamType::String,
                    CliArgKind::flag("plan"),
                    "Optional task plan payload. Leave blank for the executing agent or planning activity to author later",
                ),
                "",
            ),
            cli_only(
                "external_refs",
                ParamType::StringList,
                CliArgKind::flag("ref"),
                "External tracker reference in `system:id` form. Repeat for multiple refs",
            ),
            cli_only(
                "status",
                ParamType::Enum(TASK_CREATE_STATUS_VALUES),
                CliArgKind::flag("status"),
                "Initial task status",
            ),
            cli_only(
                "source_task",
                ParamType::String,
                CliArgKind::flag("source-task"),
                "For bug tasks: the originating task whose implementation introduced the defect",
            ),
            CLI_OPTIONS_PARAM,
        ],
    ],
    at_least_one_of: &[],
    retired_fields: ADD_RETIRED_FIELDS,
    unknown_fields: UnknownFields::Refuse,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
    id_route: McpIdRouteDeclaration::Unspecified,
};

const UPDATE_RETIRED_FIELDS: &[RetiredField] = &[
    RETIRED_AGENT_FIELD,
    RetiredField::verbatim("required_tools", REQUIRED_TOOLS_REFUSAL),
    RetiredField::verbatim("requiredTools", REQUIRED_TOOLS_REFUSAL),
    RetiredField::verbatim("required-tool", REQUIRED_TOOLS_REFUSAL),
    RetiredField::verbatim(
        "force",
        "orbit.task.update does not accept `force`; lifecycle transitions are enforced for agents, and the override is a human CLI action",
    ),
    RetiredField::verbatim(
        "artifacts",
        "orbit.task.update does not accept inline artifacts; use orbit.task.artifact with action put",
    ),
    RetiredField::unknown_field("context", "use `context_files`"),
];

const REQUIRED_TOOLS_REFUSAL: &str = "orbit.task.update does not accept `required_tools`; task tool requirements are immutable after creation";

pub(super) const UPDATE: TaskOperation = TaskOperation {
    verb: TaskVerb::Update,
    cli_path: &["update"],
    tool_name: "orbit.task.update",
    tool_description: "Update a task and return its JSON; rules are in the installed Orbit skill `references/task-fields.md`.",
    cli_about: "Update task fields, or take the next approval step with `--approve` (proposed -> backlog, review -> done)",
    cli_help: CliHelp {
        constraints: &[],
        long_about: None,
        after_help: Some(
            "Lifecycle, override, selector and scheduling rules for these flags: installed Orbit skill `references/task-fields.md` (Human CLI task update).",
        ),
        flag_order: &[
            "host",
            "id",
            "title",
            "description",
            "acceptance_criteria",
            "dependencies",
            "tags",
            "granted_tools",
            "requires_labels",
            "not_before",
            "clear_not_before",
            "plan",
            "execution_summary",
            "comment",
            "status",
            "type",
            "priority",
            "complexity",
            "planned_by",
            "implemented_by",
            "pr_status",
            "job_run_id",
            "crew",
            "orchestrator",
            "origin",
            "context_files",
            "allow_missing_context",
            "artifacts",
            "model",
            "approve",
            "note",
            "discard_candidate",
            "force",
            "allow_drop_system_tags",
            "repilot",
        ],
    },
    effect: Effect::Write,
    params: &[
        &[
            both(
                "repilot",
                ParamType::Switch,
                "Request one new pilot.",
                CliArgKind::flag("repilot"),
                "Request one new pilot after updating (operator only)",
            ),
            required(both(
                "id",
                ParamType::String,
                "Task ID.",
                CliArgKind::Positional,
                "Task ID",
            )),
            free_text(both(
                "title",
                ParamType::String,
                "New task title.",
                CliArgKind::flag("title"),
                "New title",
            )),
            free_text(preserve_empty(both(
                "description",
                ParamType::String,
                "New description; empty clears it.",
                CliArgKind::flag("description"),
                "New description (empty string clears)",
            ))),
            free_text(both(
                "acceptance_criteria",
                ParamType::StringList,
                "Replacement criteria.",
                CliArgKind::flag("acceptance-criteria"),
                "Replacement acceptance criteria. Repeat the flag for multiple criteria",
            )),
            preserve_empty(both(
                "dependencies",
                ParamType::StringList,
                "Replacement dependency IDs.",
                flag_with_delimiter("dependencies", &["dependency"], None),
                "Replacement dependency task IDs; repeat or comma-separate (empty string clears)",
            )),
            mcp_only("relations", ParamType::Array, "Replacement task relations."),
            both(
                "tags",
                ParamType::StringList,
                "Replacement task tags.",
                flag_with_delimiter("tag", &[], None),
                "Replacement task tags; repeat or comma-separate. `os:linux|macos|windows` limits the host OS",
            ),
            preserve_empty(both(
                "granted_tools",
                ParamType::StringList,
                "Replacement granted tool names.",
                flag_with_delimiter("granted-tools", &["granted-tool"], None),
                "Replacement operator-granted tool names (operator only; empty string clears)",
            )),
            preserve_empty(both(
                "requires_labels",
                ParamType::StringList,
                "Replacement capability labels.",
                flag_with_delimiter("requires-labels", &["requires-label"], Some("LABEL")),
                "Replacement labels a host must all carry to run the task (empty string clears)",
            )),
            preserve_empty(both(
                "not_before",
                ParamType::String,
                "Earliest start, or empty to clear.",
                flag_named("not-before", "TIME"),
                NOT_BEFORE_CLI_HELP,
            )),
            both(
                "allow_drop_system_tags",
                ParamType::Switch,
                "Allow dropping a system tag.",
                CliArgKind::Flag {
                    long: "allow-drop-system-tags",
                    delimiter: None,
                    aliases: &["allow-drop-system-tag", "allow-dropping-system-tags"],
                    visible_aliases: &[],
                    value_name: None,
                    multiple: false,
                },
                "Allow a tag replacement that drops a system identity tag (`ci-failure:*`)",
            ),
            free_text(preserve_empty(both(
                "plan",
                ParamType::String,
                "Replacement plan text.",
                CliArgKind::Flag {
                    long: "plan",
                    delimiter: None,
                    aliases: &["instructions"],
                    visible_aliases: &[],
                    value_name: None,
                    multiple: false,
                },
                "New task plan (empty string clears)",
            ))),
            both(
                "status",
                ParamType::Enum(TASK_UPDATE_STATUS_VALUES),
                "New task status.",
                CliArgKind::flag("status"),
                "New status (human-crew tasks accept any status, recorded as a bypass)",
            ),
            free_text(both(
                "note",
                ParamType::String,
                "Note on this status transition.",
                CliArgKind::flag("note"),
                "Note recorded on the approval's status history entry (with `--approve`)",
            )),
            both(
                "priority",
                ParamType::Enum(TASK_PRIORITY_VALUES),
                "low|medium|high|critical",
                CliArgKind::flag("priority"),
                "New dispatch priority",
            ),
            both(
                "complexity",
                ParamType::Enum(TASK_COMPLEXITY_VALUES),
                "low|medium|hard|xhard",
                CliArgKind::flag("complexity"),
                "Task complexity",
            ),
            both(
                "type",
                ParamType::Enum(TASK_TYPE_VALUES),
                "New task type.",
                flag_named("type", "TASK_TYPE"),
                "New task type",
            ),
            mcp_only(
                "source_task_id",
                ParamType::String,
                "Source bug ID; empty clears it.",
            ),
            preserve_empty(both(
                "planned_by",
                ParamType::String,
                "Planner; empty clears it.",
                CliArgKind::flag("planned-by"),
                "Explicit planning attribution label (empty string clears)",
            )),
            preserve_empty(both(
                "implemented_by",
                ParamType::String,
                "Implementer; empty clears it.",
                CliArgKind::flag("implemented-by"),
                "Explicit implementation attribution label (empty string clears)",
            )),
            free_text(preserve_empty(both(
                "execution_summary",
                ParamType::String,
                "New execution summary.",
                CliArgKind::flag("execution-summary"),
                "New execution summary (empty string clears)",
            ))),
            free_text(both(
                "comment",
                ParamType::String,
                "Comment to append.",
                CliArgKind::flag("comment"),
                "Append a task comment",
            )),
            preserve_empty(both(
                "pr_status",
                ParamType::String,
                "PR status; empty clears it.",
                CliArgKind::flag("pr-status"),
                "PR review status: `approve` or `request-changes` (empty string clears)",
            )),
            preserve_empty(both(
                "job_run_id",
                ParamType::String,
                "Job run ID; empty clears it.",
                CliArgKind::flag("job-run-id"),
                "Job run ID to associate with the task (empty string clears)",
            )),
            preserve_empty(both(
                "crew",
                ParamType::String,
                "Named crew; empty redraws.",
                CliArgKind::flag("crew"),
                "Named crew to use when running this task (empty string draws a fresh one)",
            )),
            preserve_empty(both(
                "orchestrator",
                ParamType::String,
                "Orchestrator crew; empty clears it.",
                CliArgKind::flag("orchestrator"),
                "Named crew responsible for orchestration attribution (empty string clears)",
            )),
            both(
                "origin",
                ParamType::Enum(TASK_ORIGIN_VALUES),
                "Provenance category, not a free-text reason: operator_request, orchestrator_initiative or automation. Omit it when unsure; put the reason in description.",
                CliArgKind::flag("origin"),
                "Replacement task origin (operator_request, orchestrator_initiative, automation)",
            ),
            path(preserve_empty(both(
                "context_files",
                ParamType::StringList,
                "New context selectors.",
                flag_with_delimiter("context", &["context-files"], None),
                "Replace the whole context selector list; to extend, resend existing ones too (empty string clears)",
            ))),
            both(
                "allow_missing_context",
                ParamType::Switch,
                "Accept selectors for created files.",
                CliArgKind::flag("allow-missing-context"),
                "Accept context selectors for files this task will create",
            ),
        ],
        WRITE_PROJECTION_PARAMS,
        &[
            mcp_only(
                "request_id",
                ParamType::String,
                "Retry-safe write identity.",
            ),
            mcp_only(
                "expected_revision",
                ParamType::String,
                "Revision from task show.",
            ),
            // The schema-extension table replaces the advertised shape.
            mcp_only("verdict", ParamType::Object, "Review verdict object."),
            mcp_only(
                "complete",
                ParamType::Boolean,
                "Complete an accepted review.",
            ),
            WRITE_MODEL_PARAM,
            cli_only(
                "host",
                ParamType::String,
                flag_named("host", "HOST"),
                TASK_HOST_HELP,
            ),
            cli_only(
                "clear_not_before",
                ParamType::Switch,
                CliArgKind::flag("clear-not-before"),
                "Clear the task's scheduled start, so dispatchers may start it at their next admission pass",
            ),
            cli_only(
                "artifacts",
                ParamType::StringList,
                CliArgKind::flag("artifact"),
                "Task artifact write in `path=content` form. Repeat for multiple artifacts",
            ),
            cli_only(
                "approve",
                ParamType::Switch,
                CliArgKind::flag("approve"),
                "Take the next approval step (proposed -> backlog, review -> done); no field edits or `--status`",
            ),
            cli_only(
                "discard_candidate",
                ParamType::Switch,
                CliArgKind::flag("discard-candidate"),
                "Discard the last failed run's preserved candidate; pair with `--status backlog` to requeue",
            ),
            cli_only(
                "force",
                ParamType::Switch,
                CliArgKind::flag("force"),
                "Apply `--status` even when the lifecycle refuses it (human override, recorded as `forced`)",
            ),
            CLI_OPTIONS_PARAM,
        ],
    ],
    at_least_one_of: &[],
    retired_fields: UPDATE_RETIRED_FIELDS,
    unknown_fields: UnknownFields::Refuse,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
    id_route: McpIdRouteDeclaration::Task,
};
