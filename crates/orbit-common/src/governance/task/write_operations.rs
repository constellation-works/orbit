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

use orbit_types::task::TASK_SHOW_PROJECTION_FIELDS_CSV;
use orbit_types::tool::McpToolScope;

use super::operations::{
    TASK_COMPLEXITY_VALUES, TASK_HOST_HELP, TASK_PRIORITY_VALUES, TASK_TYPE_VALUES, TaskOperation,
    TaskVerb,
};
use crate::governance::operation::{
    CliArgKind, CliHelp, Description, Effect, EnumValue, ParamSpec, ParamType, RETIRED_AGENT_FIELD,
    Redaction, RetiredField, UnknownFields, both, cli_only, cli_required, defaulted, free_text,
    mcp_only, preserve_empty, required,
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

/// The attribution field of the write tools, whose description predates the
/// shared one.
const WRITE_MODEL_PARAM: ParamSpec = both(
    "model",
    ParamType::String,
    "Preferred provenance field. Pass the canonical agent family (codex, claude, gemini, or grok); full model strings are accepted and auto-normalized.",
    CliArgKind::flag("model"),
    "Explicit agent model to persist on the task artifact",
);

/// The response projection a write takes.
pub(super) const WRITE_PROJECTION_PARAMS: &[ParamSpec] = &[
    ParamSpec {
        mcp_description: Some(Description::Computed(write_fields_description)),
        ..mcp_only("fields", ParamType::StringList, "")
    },
    mcp_only(
        "field",
        ParamType::String,
        "Compatibility alias for a single response field projection, such as `field: \"history\"`.",
    ),
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

const ADD_RETIRED_FIELDS: &[RetiredField] = &[
    RETIRED_AGENT_FIELD,
    RetiredField::unknown_field("plan", "set it with orbit.task.update"),
    RetiredField::unknown_field("status", "set it with orbit.task.update"),
    RetiredField::unknown_field("parent_id", "set it with orbit.task.update"),
    RetiredField::unknown_field("source_task_id", "set it with orbit.task.update"),
    RetiredField::unknown_field("external_refs", "set it with orbit.task.update"),
    RetiredField::unknown_field("context", "set it with orbit.task.update"),
    RetiredField::unknown_field("comment", "set it with orbit.task.update"),
    RetiredField::unknown_field("dependencies", "set it with orbit.task.update"),
];

pub(super) const ADD: TaskOperation = TaskOperation {
    verb: TaskVerb::Add,
    cli_path: &["add"],
    tool_name: "orbit.task.add",
    tool_description: "Create an Orbit task and return the created task JSON",
    cli_about: "Create a new task",
    cli_help: CliHelp {
        constraints: &[],
        long_about: None,
        after_help: None,
        flag_order: &[
            "parent_id",
            "title",
            "description",
            "acceptance_criteria",
            "dependencies",
            "tags",
            "required_tools",
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
            "model",
        ],
    },
    effect: Effect::Write,
    params: &[
        &[
            free_text(required(both(
                "title",
                ParamType::String,
                "Task title",
                CliArgKind::flag("title"),
                "Task title",
            ))),
            free_text(required(defaulted(
                both(
                    "description",
                    ParamType::String,
                    "Task description markdown",
                    CliArgKind::flag("description"),
                    "Task description",
                ),
                "",
            ))),
            // The binding key for the task store; an MCP session's own
            // workspace may supply it, and process cwd never does.
            mcp_only(
                "workspace",
                ParamType::String,
                "Workspace selector: a registered workspace name, a logical workspace ID (`ws_*`), or an absolute path to a local checkout (a linked Git worktree resolves to its registered checkout). Falls back to the MCP session's `_meta.orbit.workspace` when omitted; never inferred from process cwd.",
            ),
            free_text(both(
                "acceptance_criteria",
                ParamType::StringList,
                "Optional acceptance criteria as a string or array of strings",
                CliArgKind::flag("acceptance-criteria"),
                "Acceptance criteria. Repeat the flag for multiple criteria",
            )),
            both(
                "tags",
                ParamType::StringList,
                "Optional tags as a string or array of strings. `os:linux`, `os:macos` or `os:windows` limits which host OS may run the task (several: any one of them); any other `os:` value is rejected",
                flag_with_delimiter("tag", &[], None),
                "Task tags. Repeat or comma-separate for multiple tags. `os:linux`, `os:macos` or `os:windows` limits which host OS may run the task; any other `os:` value is rejected",
            ),
            both(
                "required_tools",
                ParamType::StringList,
                "Optional exact canonical tool names the task adds to its agent activity baseline, as a string or array of strings",
                flag_with_delimiter("required-tools", &["required-tool"], None),
                "Exact canonical tool names the task adds to its agent activity baseline",
            ),
            path(both(
                "context_files",
                ParamType::StringList,
                "Optional task context selectors as a comma-separated string or array of strings. Add entries ONLY for existing files, directories, or symbols expected to be modified or deleted by the task. Do not add background-reading entries or files referenced only for context. Prefer canonical selectors: `file:`, `dir:`, or `symbol:path#name:kind`. Legacy raw paths are accepted and upgraded automatically. Existence checks verify the filesystem anchor only; a `symbol:` name and kind are not looked up.",
                flag_with_delimiter("context", &[], Some("CONTEXT")),
                "Task context selectors. Repeat or comma-separate for multiple selectors. Prefer `file:`, `dir:`, or `symbol:` forms; legacy raw paths are accepted and upgraded. Existence checks verify the filesystem anchor only; a `symbol:` name and kind are not looked up",
            )),
            both(
                "allow_missing_context",
                ParamType::Switch,
                "Optional. Set true to accept `context_files` selectors whose target does not exist yet, for work that creates the file. Each such selector is recorded on the task as durable creation intent for that exact selector, which later writes and task preparation honor. Syntax, workspace containment and file/directory kind are still checked. Missing selectors are rejected by default.",
                CliArgKind::flag("allow-missing-context"),
                "Accept context selectors whose target does not exist yet (for work that creates the file). Each missing selector is recorded as durable creation intent for the task",
            ),
            defaulted(
                both(
                    "priority",
                    ParamType::Enum(TASK_PRIORITY_VALUES),
                    "Optional priority level",
                    CliArgKind::flag("priority"),
                    "Priority level",
                ),
                "medium",
            ),
            cli_required(both(
                "complexity",
                ParamType::Enum(TASK_COMPLEXITY_VALUES),
                "Required for ordinary creation; omit for guarded request_id creation. Task complexity level (low, medium, hard, or xhard). Accepted aliases: easy, small, or trivial → low; large or big → hard",
                CliArgKind::flag("complexity"),
                "Task complexity (low, medium, hard, or xhard)",
            )),
            both(
                "type",
                ParamType::Enum(TASK_TYPE_VALUES),
                "Optional task type",
                flag_named("type", "TASK_TYPE"),
                "Task type",
            ),
            mcp_only(
                "relations",
                ParamType::Array,
                "Optional typed task relations as an array of {type, target} objects",
            ),
            both(
                "crew",
                ParamType::String,
                "Optional named crew to use when running this task; omitted, one is drawn from the complexity pool or the configured default",
                CliArgKind::flag("crew"),
                "Named crew to use when running this task (default: drawn from the complexity pool, else the configured default crew)",
            ),
            both(
                "orchestrator",
                ParamType::String,
                "Optional named crew responsible for orchestration attribution; does not select execution. Defaults to the MCP session's `orbit mcp serve --orchestrator` crew when omitted",
                CliArgKind::flag("orchestrator"),
                "Named crew responsible for orchestration attribution",
            ),
        ],
        WRITE_PROJECTION_PARAMS,
        &[
            mcp_only(
                "request_id",
                ParamType::String,
                "Optional retry-safe proposed creation. Requires explicit workspace and acceptance_criteria; accepts only title, description, acceptance_criteria, priority and crew. Reuse the identity with identical input to reconcile a lost reply. Returns a versioned snapshot.",
            ),
            WRITE_MODEL_PARAM,
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
        "orbit.task.update does not accept inline artifacts; use orbit.task.artifact.put",
    ),
];

const REQUIRED_TOOLS_REFUSAL: &str = "orbit.task.update does not accept `required_tools`; task tool requirements are immutable after creation";

pub(super) const UPDATE: TaskOperation = TaskOperation {
    verb: TaskVerb::Update,
    cli_path: &["update"],
    tool_name: "orbit.task.update",
    tool_description: "Update an Orbit task and return the fresh task JSON. Field edits may accompany a status change, except `status: backlog` on a proposed task (approval), which accepts only `note` and `comment`. Starting with `status: in-progress` from a pickup state may include field edits on the same write.",
    cli_about: "Update task fields, or take the next approval step with `--approve` (proposed -> backlog, review -> done)",
    cli_help: CliHelp {
        constraints: &[],
        long_about: None,
        after_help: None,
        flag_order: &[
            "host",
            "id",
            "title",
            "description",
            "acceptance_criteria",
            "dependencies",
            "tags",
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
            "context_files",
            "allow_missing_context",
            "artifacts",
            "model",
            "approve",
            "note",
            "discard_candidate",
            "force",
            "allow_drop_system_tags",
        ],
    },
    effect: Effect::Write,
    params: &[
        &[
            required(both(
                "id",
                ParamType::String,
                "task ID",
                CliArgKind::Positional,
                "Task ID",
            )),
            free_text(both(
                "title",
                ParamType::String,
                "New task title",
                CliArgKind::flag("title"),
                "New title",
            )),
            free_text(preserve_empty(both(
                "description",
                ParamType::String,
                "New task description (empty string clears)",
                CliArgKind::flag("description"),
                "New description (empty string clears)",
            ))),
            free_text(both(
                "acceptance_criteria",
                ParamType::StringList,
                "New acceptance criteria as an array of strings or a single string",
                CliArgKind::flag("acceptance-criteria"),
                "Replacement acceptance criteria. Repeat the flag for multiple criteria",
            )),
            preserve_empty(both(
                "dependencies",
                ParamType::StringList,
                "Replacement dependency task IDs as a string or array of strings",
                flag_with_delimiter("dependencies", &["dependency"], None),
                "Replacement dependency task IDs. Repeat or comma-separate for multiple dependencies (empty string clears)",
            )),
            mcp_only(
                "relations",
                ParamType::Array,
                "Replacement typed task relations as an array of {type, target} objects",
            ),
            both(
                "tags",
                ParamType::StringList,
                "Replacement task tags as a string or array of strings. `os:linux`, `os:macos` or `os:windows` limits which host OS may run the task from its next admission; any other `os:` value is rejected",
                flag_with_delimiter("tag", &[], None),
                "Replacement task tags. Repeat or comma-separate for multiple tags. `os:linux`, `os:macos` or `os:windows` limits which host OS may run the task from its next admission; any other `os:` value is rejected",
            ),
            both(
                "allow_drop_system_tags",
                ParamType::Switch,
                "Optional. Set true to permit replacing `tags` with a list that drops a system identity tag (`ci-failure:*`). Missing or false rejects the tag drop.",
                CliArgKind::Flag {
                    long: "allow-drop-system-tags",
                    delimiter: None,
                    aliases: &["allow-drop-system-tag", "allow-dropping-system-tags"],
                    visible_aliases: &[],
                    value_name: None,
                    multiple: false,
                },
                "Allow replacing tags when the replacement list drops a system identity tag (`ci-failure:*`)",
            ),
            free_text(preserve_empty(both(
                "plan",
                ParamType::String,
                "Replacement task plan text (empty string clears). May be supplied on the same write that transitions to in-progress when a plan is required.",
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
                "New task status. `backlog` on a proposed task is the approval transition and cannot be combined with field edits (only `note` and `comment`). `in-progress` from a pickup state is the start transition and may include field edits on the same write; from any other status that start is refused whether or not extra fields are present. Other status changes are ordinary governed updates and may include field edits.",
                CliArgKind::flag("status"),
                "New status",
            ),
            free_text(both(
                "note",
                ParamType::String,
                "Annotation stored on this status transition's history entry; use `comment` for free-form discussion",
                CliArgKind::flag("note"),
                "Note recorded on the approval's status history entry (with `--approve`)",
            )),
            both(
                "priority",
                ParamType::Enum(TASK_PRIORITY_VALUES),
                "New dispatch priority (low, medium, high, or critical)",
                CliArgKind::flag("priority"),
                "New dispatch priority",
            ),
            both(
                "complexity",
                ParamType::Enum(TASK_COMPLEXITY_VALUES),
                "Optional task complexity level (low, medium, hard, or xhard). Accepted aliases: easy, small, or trivial → low; large or big → hard",
                CliArgKind::flag("complexity"),
                "Task complexity",
            ),
            both(
                "type",
                ParamType::Enum(TASK_TYPE_VALUES),
                "New task type",
                flag_named("type", "TASK_TYPE"),
                "New task type",
            ),
            mcp_only(
                "source_task_id",
                ParamType::String,
                "For bug tasks: originating task ID that introduced the defect (empty string clears)",
            ),
            preserve_empty(both(
                "planned_by",
                ParamType::String,
                "Explicit planning attribution label (empty string clears)",
                CliArgKind::flag("planned-by"),
                "Explicit planning attribution label (empty string clears)",
            )),
            preserve_empty(both(
                "implemented_by",
                ParamType::String,
                "Explicit implementation attribution label (empty string clears)",
                CliArgKind::flag("implemented-by"),
                "Explicit implementation attribution label (empty string clears)",
            )),
            free_text(preserve_empty(both(
                "execution_summary",
                ParamType::String,
                "Replacement execution summary text",
                CliArgKind::flag("execution-summary"),
                "New execution summary (empty string clears)",
            ))),
            free_text(both(
                "comment",
                ParamType::String,
                "Task comment to append",
                CliArgKind::flag("comment"),
                "Append a task comment",
            )),
            preserve_empty(both(
                "pr_status",
                ParamType::String,
                "PR review status (e.g. approve, request-changes; empty string clears)",
                CliArgKind::flag("pr-status"),
                "PR review status: `approve` or `request-changes` (empty string clears)",
            )),
            preserve_empty(both(
                "job_run_id",
                ParamType::String,
                "Job run ID to associate with the task (empty string clears)",
                CliArgKind::flag("job-run-id"),
                "Job run ID to associate with the task (empty string clears)",
            )),
            preserve_empty(both(
                "crew",
                ParamType::String,
                "Named crew to use when running this task (empty string draws a fresh one)",
                CliArgKind::flag("crew"),
                "Named crew to use when running this task (empty string draws a fresh one)",
            )),
            preserve_empty(both(
                "orchestrator",
                ParamType::String,
                "Named crew responsible for orchestration attribution (empty string clears; mutable only before execution)",
                CliArgKind::flag("orchestrator"),
                "Named crew responsible for orchestration attribution (empty string clears)",
            )),
            path(preserve_empty(both(
                "context_files",
                ParamType::StringList,
                "Replacement task context selectors as a comma-separated string or array of strings. Omit both `context_files` and `context` to preserve the current list. A supplied value replaces the whole list; it does not append. An empty array `[]` clears the list, as does a comma-only string such as `\",\"`; an empty string is rejected. The string `\"[]\"` is treated as a selector, not an empty list. To extend scope, read the current list and send the full union of existing and new selectors. Add entries ONLY for existing files, directories, or symbols expected to be modified or deleted by the task. Do not add background-reading entries or files referenced only for context. Prefer canonical selectors: `file:path`, `dir:path`, or `symbol:path#name:kind`. Legacy raw paths are accepted and upgraded automatically. Existence checks verify the filesystem anchor only; a `symbol:` name and kind are not looked up.",
                flag_with_delimiter("context", &["context-files"], None),
                "Replace the whole task context list. Omit to preserve it; an empty string clears it. To extend scope, include all existing selectors plus the new ones. Repeat or comma-separate for multiple selectors. Prefer `file:`, `dir:`, or `symbol:` forms; legacy raw paths are accepted and upgraded. Existence checks verify the filesystem anchor only; a `symbol:` name and kind are not looked up",
            ))),
            both(
                "allow_missing_context",
                ParamType::Switch,
                "Optional. Set true to accept `context_files` selectors whose target does not exist yet, for work that creates the file. Each such selector is recorded on the task as durable creation intent for that exact selector, which later writes and task preparation honor; re-sending a declared selector needs no flag, and a replacement list that omits it revokes it. Syntax, workspace containment and file/directory kind are still checked. Missing selectors are rejected by default.",
                CliArgKind::flag("allow-missing-context"),
                "Accept context selectors whose target does not exist yet (for work that creates the file). Each missing selector is recorded as durable creation intent; dropping a selector from the list revokes it, and re-sending a recorded one needs no flag",
            ),
            path(mcp_only(
                "context",
                ParamType::String,
                "Legacy string alias for `context_files`. Omit both fields to preserve the current list. A supplied value replaces the whole list; it does not append. A comma-only string such as `\",\"` clears the list; prefer `context_files: []` to clear explicitly. An empty string is rejected, and the string `\"[]\"` is treated as a selector, not an empty list. `context_files` takes precedence when both fields are supplied. To extend scope, read the current list and send the full union of existing and new selectors. Add entries ONLY for existing files, directories, or symbols expected to be modified or deleted by the task. Do not add background-reading entries or files that are only relevant background context. Prefer canonical selectors: `file:path`, `dir:path`, or `symbol:path#name:kind`. Existence checks verify the filesystem anchor only; a `symbol:` name and kind are not looked up.",
            )),
        ],
        WRITE_PROJECTION_PARAMS,
        &[
            mcp_only(
                "request_id",
                ParamType::String,
                "Optional retry-safe guarded write identity. Requires explicit workspace and expected_revision. Reuse identical input to reconcile a lost reply; returns a fresh versioned snapshot.",
            ),
            mcp_only(
                "expected_revision",
                ParamType::String,
                "Observed revision from task.show snapshot:true. Guarded mode accepts field edits, a comment, or a verdict separately.",
            ),
            // The schema-extension table replaces the advertised shape.
            mcp_only(
                "verdict",
                ParamType::Object,
                "Evidence-bound review verdict: decision (accept or changes_requested), rationale, criteria [{criterion,met,evidence}], evidence, expected_run_id and expected_head. Review requires request_id and expected_revision.",
            ),
            mcp_only(
                "complete",
                ParamType::Boolean,
                "Guarded review only: explicitly complete an accepted review with existing trusted operator authority. Default false; never merges, publishes or dispatches.",
            ),
            WRITE_MODEL_PARAM,
            cli_only(
                "host",
                ParamType::String,
                flag_named("host", "HOST"),
                TASK_HOST_HELP,
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
                "Take the task's next approval step: `proposed` -> `backlog`, or `review` -> `done`. The transition is chosen from the current status, so it cannot be combined with field edits or an explicit `--status`",
            ),
            cli_only(
                "discard_candidate",
                ParamType::Switch,
                CliArgKind::flag("discard-candidate"),
                "Discard the candidate the task's last failed run preserved, so its next run implements fresh instead of resuming it. Combine with `--status backlog` to requeue",
            ),
            cli_only(
                "force",
                ParamType::Switch,
                CliArgKind::flag("force"),
                "Apply `--status` even when the task lifecycle refuses the transition (for example reopening a done task). Human-operator override: the change is recorded in task history as `forced`",
            ),
            CLI_OPTIONS_PARAM,
        ],
    ],
    at_least_one_of: &[],
    retired_fields: UPDATE_RETIRED_FIELDS,
    unknown_fields: UnknownFields::Refuse,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
};

fn write_fields_description() -> String {
    format!(
        "Optional response field projection as a string or array. When omitted, write responses exclude append-heavy `comments` and `history`; request those fields explicitly when needed. Valid values: {TASK_SHOW_PROJECTION_FIELDS_CSV}."
    )
}
