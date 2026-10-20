//! The task operation registry — every task tool, declared once.
//!
//! The read verbs (`list`, `eligible`, `show`, `lint`) are declared here, the
//! write verbs (`add`, `update`) in `write_operations.rs`, the artifact and
//! lock verbs in `nested_operations.rs`, and the review and lifecycle verbs
//! in `lifecycle_operations.rs`. Every task tool is declared; the CLI's
//! `orbit task` mixes them with subcommands that have no tool.
//!
//! The CLI and MCP shapes of a verb may differ. `show` takes `--host`, which
//! the CLI resolves before any runtime opens, and `list` takes the CLI-only
//! `--all`, `--ops` and `--full`. MCP-only parameters (bounded views,
//! snapshots, projections) have no binding. Where `--help` lists flags in an
//! order the MCP schema does not, [`CliHelp::flag_order`] keeps both.
//!
//! **Every string below is shipped contract.** Tool names and parameter names
//! are the MCP wire; CLI flag spellings and help text are the argv surface.

use orbit_types::task::{TASK_SHOW_DELIVERY_FIELD, TASK_SHOW_PROJECTION_FIELDS_CSV};
use orbit_types::tool::McpToolScope;

use super::lifecycle_operations::{
    DELETE, RECONCILE_REVIEW, REJECT, REQUEUE, RETRY_HANDOFF, REVIEW_RESET,
};
use super::nested_operations::{
    ARTIFACT_GET, ARTIFACT_PUT, LOCKS_LIST, LOCKS_RELEASE, LOCKS_RESERVE,
};
use super::write_operations::{ADD, UPDATE};
use crate::governance::operation::{
    CliArgKind, CliGroup, CliHelp, Description, Effect, EnumValue, NounSpec, OperationSpec,
    ParamSpec, ParamType, ToolOnlyRegistration, UnknownFields, aliased, both, cli_only, defaulted,
    mcp_only, operation_registry,
};

operation_registry! {
    noun: TASK;
    /// Every verb of the task noun: one per task tool.
    ///
    /// The join between the spec table here and the handler table in
    /// `orbit-core`: a new variant fails to compile in every exhaustive
    /// surface table until the verb is wired.
    pub enum TaskVerb;
    /// The task operations. `orbit task --help` lists every task
    /// command from its own template, so this order is not `--help` order.
    /// Within a spec, parameter order is the MCP tool schema order.
    pub const TASK_OPERATIONS;
    {
        /// Create a task.
        Add => ADD,
        /// Update a task's fields, or move it through its lifecycle.
        Update => UPDATE,
        /// List tasks.
        List => LIST,
        /// List the backlog and proposed tasks no in-flight work blocks.
        Eligible => ELIGIBLE,
        /// Show one task.
        Show => SHOW,
        /// Lint one task, or sweep active tasks for context needing repair.
        Lint => LINT,
        /// Store an artifact.
        ArtifactPut => ARTIFACT_PUT,
        /// Read an artifact.
        ArtifactGet => ARTIFACT_GET,
        /// List active locks.
        LocksList => LOCKS_LIST,
        /// Reserve a lock surface.
        LocksReserve => LOCKS_RESERVE,
        /// Release a reservation.
        LocksRelease => LOCKS_RELEASE,
        /// Renew one review lineage's budget, as an operator decision.
        ReviewReset => REVIEW_RESET,
        /// Validate and review a recovered delivery's changed merged head.
        ReconcileReview => RECONCILE_REVIEW,
        /// Requeue a blocked task onto its published candidate, as an
        /// operator decision.
        Requeue => REQUEUE,
        /// Retry a stopped or uncertain authorized owner handoff.
        RetryHandoff => RETRY_HANDOFF,
        /// Reject a task.
        Reject => REJECT,
        /// Permanently delete a task.
        Delete => DELETE,
    }
}

/// The task noun.
pub const TASK: NounSpec<TaskVerb> = NounSpec {
    cli_name: "task",
    tool_prefix: "orbit.task",
    cli_about: "Create, update, and manage tasks",
    audit_target_type: "task",
    audit_target_param: "id",
    tool_only_registration: ToolOnlyRegistration::Inactive,
    cli_groups: &[
        CliGroup {
            name: "artifact",
            cli_about: "Manage task artifact files",
            params: &[cli_only(
                "host",
                ParamType::String,
                CliArgKind::Flag {
                    long: "host",
                    delimiter: None,
                    aliases: &[],
                    visible_aliases: &[],
                    value_name: Some("HOST"),
                    multiple: false,
                },
                TASK_HOST_HELP,
            )],
        },
        CliGroup {
            name: "locks",
            cli_about: "Inspect, reserve, and release task file locks",
            params: &[],
        },
    ],
    operations: TASK_OPERATIONS,
};

/// A task operation specification.
pub type TaskOperation = OperationSpec<TaskVerb>;

/// `TaskType` as the CLI offers it.
pub const TASK_TYPE_VALUES: &[EnumValue] = &[
    EnumValue::plain("feature"),
    EnumValue::helped(
        "bug",
        "An attributable defect; lineage is recorded through typed task relations",
    ),
    EnumValue::plain("refactor"),
    EnumValue::plain("chore"),
];

/// `TaskOrigin` as the CLI offers it.
pub const TASK_ORIGIN_VALUES: &[EnumValue] = &[
    EnumValue::helped("operator_request", "A human operator asked for the work"),
    EnumValue::helped(
        "orchestrator_initiative",
        "An orchestrator decided the work was needed on its own",
    ),
    EnumValue::helped(
        "automation",
        "Filed by automation: an auto-task, routine, sweep or other system filer",
    ),
];

/// `TaskPriority` as the CLI offers it.
pub const TASK_PRIORITY_VALUES: &[EnumValue] = &[
    EnumValue::plain("low"),
    EnumValue::plain("medium"),
    EnumValue::plain("high"),
    EnumValue::plain("critical"),
];

/// The assessable `TaskComplexity` values, as the CLI offers them.
pub const TASK_COMPLEXITY_VALUES: &[EnumValue] = &[
    EnumValue::plain("low"),
    EnumValue::plain("medium"),
    EnumValue::plain("hard"),
    EnumValue::helped(
        "xhard",
        "Reserved top tier for the most capable (and most expensive) crews",
    ),
];

/// `TaskStatus` as the CLI offers it.
pub const TASK_STATUS_VALUES: &[EnumValue] = &[
    EnumValue::helped(
        "proposed",
        "Awaiting human approval before entering the backlog",
    ),
    EnumValue::helped("backlog", "Approved and queued for work; not yet started"),
    EnumValue {
        name: "in-progress",
        help: Some("Actively being worked on"),
        aliases: &["in_progress"],
    },
    EnumValue::helped("review", "Implementation complete; awaiting review/merge"),
    EnumValue::helped(
        "done",
        "Accepted and closed. May be reopened by an explicit status edit",
    ),
    EnumValue::helped(
        "blocked",
        "Temporarily paused (waiting on a dependency or decision)",
    ),
    EnumValue::helped(
        "archived",
        "Soft-deleted. Can be restored to any other status",
    ),
    EnumValue::helped(
        "rejected",
        "Declined. Can be reclassified to any other status",
    ),
    EnumValue::helped(
        "someday",
        "Future-scoped — wanted but not yet actionable. Agents skip someday tasks",
    ),
];

/// The statuses a task can be eligible from: work nobody has taken yet.
const ELIGIBLE_STATUS_VALUES: &[EnumValue] =
    &[EnumValue::plain("backlog"), EnumValue::plain("proposed")];

/// `--help` for the `--host` flag every id-routed task command takes.
pub const TASK_HOST_HELP: &str = "Host that `--workspace` names, by registered host name or `machine_id` (see `orbit host list`). Orbit reads that host's live workspace list and uses the selector it lists for the workspace. Without it, a task id goes to the host its prefix names";

/// The attribution fields every task tool accepts.
pub(super) const IDENTITY_PARAMS: &[ParamSpec] = &[
    mcp_only(
        "agent",
        ParamType::String,
        "Deprecated compatibility field. Prefer `model` with the agent family (`codex`, `claude`, `gemini`, or `grok`).",
    ),
    mcp_only(
        "model",
        ParamType::String,
        "Preferred provenance field. Pass the canonical agent family (`codex`, `claude`, `gemini`, or `grok`); full model strings are accepted and auto-normalized.",
    ),
];

/// The `view: "bounded"` selector of the dashboard's bounded reads.
const BOUNDED_VIEW_PARAM: ParamSpec = mcp_only(
    "view",
    ParamType::String,
    "bounded: bounded metadata, explicit workspace, pagination and display truncation. Omit for the existing full-record response.",
);

const LIST: TaskOperation = TaskOperation {
    verb: TaskVerb::List,
    cli_path: &["list"],
    tool_name: "orbit.task.list",
    tool_description: "List Orbit tasks in a {tasks, total, truncated} envelope (default limit 50). With no status filter, non-terminal tasks come first and terminal tasks follow, each newest first; an explicit status filter is newest first. `total` counts all matching tasks and `truncated` reports whether the limit omitted tasks. Optional filters: status, parent, type, origin, tag, dependency readiness, path, runs_on. os_requirement is derived from OS tags intersected with the stored crew executor host_os, with source and unresolved/conflict states.",
    cli_about: "List tasks with optional filters",
    cli_help: CliHelp {
        constraints: &[],
        long_about: None,
        after_help: Some(
            "Examples:\n  orbit task list\n  orbit task list --limit 100\n  orbit task list --status backlog\n  orbit task list --status in-progress,review\n  orbit task list --type feature\n  orbit task list --origin operator_request\n  orbit task list --priority high\n  orbit task list --parent T12345678-123456\n  orbit task list --ref jira:ENG-1234\n  orbit task list --has-ref jira\n  orbit task list --tag perf --tag bench\n  orbit task list --path src/auth/login.rs\n  orbit task list --json",
        ),
        flag_order: &[
            "status",
            "all",
            "limit",
            "priority",
            "type",
            "origin",
            "parent_id",
            "job_run_id",
            "tag",
            "external_ref",
            "has_ref",
            "ready",
            "runs_on",
            "path",
            "ops",
            "full",
        ],
    },
    effect: Effect::Read,
    params: &[
        &[
            both(
                "status",
                ParamType::EnumList(TASK_STATUS_VALUES),
                "Optional task status filter. Pass a comma-separated string or an array of status names.",
                CliArgKind::Flag {
                    long: "status",
                    delimiter: Some(','),
                    aliases: &[],
                    visible_aliases: &[],
                    value_name: None,
                    multiple: false,
                },
                "Filter by one or more statuses (comma-separated). Opt-in: with no `--status`, tasks of every lifecycle status are listed under a status-aware rule (non-terminal tasks first, then terminal tasks)",
            ),
            aliased(
                both(
                    "parent_id",
                    ParamType::String,
                    "Optional parent task ID to list subtasks for",
                    CliArgKind::flag("parent"),
                    "Filter to subtasks belonging to a parent task",
                ),
                &["parent", "parentId"],
            ),
            aliased(
                both(
                    "type",
                    ParamType::Enum(TASK_TYPE_VALUES),
                    "Optional task type filter",
                    CliArgKind::Flag {
                        long: "type",
                        delimiter: None,
                        aliases: &[],
                        visible_aliases: &[],
                        value_name: Some("TASK_TYPE"),
                        multiple: false,
                    },
                    "Filter by task type (feature, bug, refactor, chore)",
                ),
                &["task_type", "taskType"],
            ),
            both(
                "origin",
                ParamType::Enum(TASK_ORIGIN_VALUES),
                "Optional task origin filter: operator_request, orchestrator_initiative or automation. Tasks without a recorded origin never match.",
                CliArgKind::flag("origin"),
                "Filter by task origin (operator_request, orchestrator_initiative, automation); tasks without a recorded origin never match",
            ),
            both(
                "job_run_id",
                ParamType::String,
                "Filter by job run ID",
                CliArgKind::flag("job-run-id"),
                "Filter by job run ID",
            ),
            aliased(
                both(
                    "tag",
                    ParamType::StringList,
                    "Filter by tag. Repeat or pass an array for AND semantics.",
                    CliArgKind::Flag {
                        long: "tag",
                        delimiter: Some(','),
                        aliases: &[],
                        visible_aliases: &[],
                        value_name: Some("TAGS"),
                        multiple: false,
                    },
                    "Filter by tag. Repeat for AND semantics",
                ),
                &["tags"],
            ),
            both(
                "runs_on",
                ParamType::Enum(&[
                    EnumValue::helped("linux", "Linux hosts"),
                    EnumValue::helped("macos", "macOS hosts"),
                    EnumValue::helped("windows", "Windows hosts"),
                ]),
                "Filter by effective host OS: linux, macos or windows. Intersects OS tags with the stored crew executor. Unrestricted tasks match; unresolved crews and conflicts do not.",
                CliArgKind::flag("runs-on"),
                "Filter by effective host OS: tags intersected with the stored crew executor; unrestricted tasks match, unresolved crews and conflicts do not",
            ),
            both(
                "ready",
                ParamType::Switch,
                "When true, keep only tasks whose dependencies are satisfied",
                CliArgKind::flag("ready"),
                "Keep only tasks whose dependencies are already satisfied",
            ),
            both(
                "path",
                ParamType::String,
                "Filter to tasks whose `context_files` selectors apply to this path. Supports file:, dir:, symbol:, and bare path selectors; bidirectional containment.",
                CliArgKind::flag("path"),
                "Filter to tasks whose `context_files` selectors apply to this path. Selector forms supported: `file:`, `dir:`, `symbol:`, and bare paths. Bidirectional containment — passing a directory matches every selector under it",
            ),
            defaulted(
                both(
                    "limit",
                    ParamType::Integer,
                    "Maximum number of tasks to return (default 50). With no status filter, non-terminal tasks come first and terminal tasks follow, each newest first. Must be at least 1.",
                    CliArgKind::flag("limit"),
                    "Maximum number of tasks to return (default 50). Must be at least 1. Under the default status-aware rule, non-terminal tasks are listed first (newest first), followed by terminal tasks (newest first)",
                ),
                "50",
            ),
            mcp_only(
                "fields",
                ParamType::StringList,
                "Optional per-task field projection as a string or array, using the field names `orbit.task.show` accepts (for example `id`, `title`, `status`). When set, each listed task is an object holding only those fields; omit it for full records. Full records include descriptions, plans and summaries, so a long list can be very large: project the few fields a scan needs and fetch details with `orbit.task.show`.",
            ),
            BOUNDED_VIEW_PARAM,
            mcp_only("offset", ParamType::Integer, "Bounded view list offset"),
            mcp_only(
                "search",
                ParamType::String,
                "Bounded view key/title substring",
            ),
            both(
                "priority",
                ParamType::Enum(TASK_PRIORITY_VALUES),
                "Optional task priority filter",
                CliArgKind::flag("priority"),
                "Filter by priority level",
            ),
            both(
                "external_ref",
                ParamType::String,
                "Optional exact external reference filter in `system:id` form",
                CliArgKind::flag("ref"),
                "Filter by exact external reference in `system:id` form",
            ),
            both(
                "has_ref",
                ParamType::String,
                "Optional external reference system filter: keep tasks with any reference in this system",
                CliArgKind::flag("has-ref"),
                "Filter by external reference system",
            ),
            cli_only(
                "all",
                ParamType::Switch,
                CliArgKind::flag("all"),
                "Deprecated no-op: task listing is status-neutral by default, so `--all` is no longer required to see every lifecycle status. Accepted for backward compatibility and ignored",
            ),
            cli_only(
                "ops",
                ParamType::Switch,
                CliArgKind::flag("ops"),
                "Output signal-tier JSON (id, title, type, status, priority only)",
            ),
            cli_only(
                "full",
                ParamType::Switch,
                CliArgKind::flag("full"),
                "Show all table columns in text output",
            ),
        ],
        IDENTITY_PARAMS,
    ],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
};

const ELIGIBLE: TaskOperation = TaskOperation {
    verb: TaskVerb::Eligible,
    cli_path: &["eligible"],
    tool_name: "orbit.task.eligible",
    tool_description: "List backlog and proposed tasks that can be picked up now without colliding with work in flight, as {tasks, total, truncated[, conflicting]}. A task is eligible when its context-file lock surface overlaps no in-progress or review task's surface, decided by the same lock code automatic dispatch uses. No other gate applies: dependencies, complexity, groups, crew and overlap between candidates are not checked. Read-only.",
    cli_about: "List backlog/proposed tasks you can pick up without colliding with work in flight",
    cli_help: CliHelp {
        constraints: &[],
        long_about: Some(
            "List backlog/proposed tasks you can pick up without colliding with work in flight\n\nA task is eligible when its context-file lock surface overlaps no in-progress or review task's surface — the same lock test automatic dispatch applies. Nothing else is checked: not dependencies, complexity, groups or crew, and candidates are not checked against each other. Read-only: nothing is reserved, reconciled or changed.",
        ),
        after_help: Some(
            "Examples:\n  orbit task eligible\n  orbit task eligible --status proposed\n  orbit task eligible --path crates/orbit-cli\n  orbit task eligible --explain\n  orbit task eligible --json",
        ),
        flag_order: &[],
    },
    effect: Effect::Read,
    params: &[
        &[
            both(
                "status",
                ParamType::EnumList(ELIGIBLE_STATUS_VALUES),
                "Candidate statuses: `backlog`, `proposed`, or both (the default). Pass a comma-separated string or an array.",
                CliArgKind::Flag {
                    long: "status",
                    delimiter: Some(','),
                    aliases: &[],
                    visible_aliases: &[],
                    value_name: None,
                    multiple: false,
                },
                "Candidate statuses (comma-separated). Default: backlog and proposed",
            ),
            both(
                "path",
                ParamType::String,
                "Keep only candidates whose `context_files` selectors apply to this path, matched as `orbit.task.list` matches `path`.",
                CliArgKind::flag("path"),
                "Keep only candidates whose `context_files` selectors apply to this path, matched as `orbit task list --path` matches",
            ),
            defaulted(
                both(
                    "limit",
                    ParamType::Integer,
                    "Maximum eligible tasks to return (default 50). Must be at least 1.",
                    CliArgKind::flag("limit"),
                    "Maximum eligible tasks to list (default 50). Must be at least 1",
                ),
                "50",
            ),
            both(
                "explain",
                ParamType::Switch,
                "When true, also return `conflicting`: every held-back candidate with the overlapping selector and the in-progress or review task holding it.",
                CliArgKind::flag("explain"),
                "Also list the held-back candidates, each with the overlapping file and the in-progress or review task holding it",
            ),
        ],
        IDENTITY_PARAMS,
    ],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
};

const SHOW: TaskOperation = TaskOperation {
    verb: TaskVerb::Show,
    cli_path: &["show"],
    tool_name: "orbit.task.show",
    tool_description: "Fetch a single Orbit task as JSON. `id` is a globally unique primary key resolved through the host task registry by default; cwd, MCP initialize metadata, and linked-worktree runtime identities are not used as filters. An optional `workspace` argument is an explicit fail-closed filter only. Use the optional `fields` projection (or single-field alias `field`) to retrieve only specific task fields, including the derived read-only `terminal` field. `os_requirement` reports tags intersected with the stored crew executor host_os, its source, and unresolved/conflict states; it is never persisted. The `crew` field selects execution, while `orchestrator` records orchestration attribution. `field: \"delivery\"` (optional `run_id`) reports what a delivery run committed and landed for the task: typed status, base/head and landed commit SHAs, PR number, timestamps and provenance only, from the host's own commit and merge step records. Missing or inconsistent evidence is reported as unavailable, never inferred. Full run details stay on the operator-only `orbit.workflow.run.show`.",
    cli_about: "Show detailed information about a task, found by ID with an optional workspace filter",
    cli_help: CliHelp {
        long_about: Some(
            "Show a task by ID. Runs on reports OS tags intersected with the stored crew executor host_os, naming the source or an unresolved/conflicting constraint. This readout is derived and never stored. An unset crew leaves automatic routing undecided.",
        ),
        ..CliHelp::NONE
    },
    effect: Effect::Read,
    params: &[
        &[
            cli_only(
                "host",
                ParamType::String,
                CliArgKind::Flag {
                    long: "host",
                    delimiter: None,
                    aliases: &[],
                    visible_aliases: &[],
                    value_name: Some("HOST"),
                    multiple: false,
                },
                TASK_HOST_HELP,
            ),
            TASK_ID_PARAM,
            mcp_only(
                "snapshot",
                ParamType::Boolean,
                "True returns a bounded versioned snapshot with opaque revision and available actions. Requires explicit workspace; snapshot grants no authority and cannot combine with field projections.",
            ),
        ],
        IDENTITY_PARAMS,
        &[
            ParamSpec {
                name: "fields",
                aliases: &[],
                param_type: ParamType::StringList,
                required: false,
                preserve_empty: false,
                default: None,
                redact: crate::governance::operation::Redaction::None,
                mcp_description: Some(Description::Computed(show_fields_description)),
                cli: Some(crate::governance::operation::CliBinding {
                    required: false,
                    kind: CliArgKind::Flag {
                        long: "fields",
                        delimiter: Some(','),
                        aliases: &["field"],
                        visible_aliases: &[],
                        value_name: None,
                        multiple: true,
                    },
                    help: Description::Static(concat!(
                        "Print only the specified field projection(s). Valid values: ",
                        orbit_types::task_show_projection_fields_csv!(),
                        ". Repeat the flag or use a comma-separated value list. Combined with --json, \
                         a single field returns that field as JSON and multiple fields return a JSON object. \
                         The derived terminal field remains keyed as an object."
                    )),
                }),
            },
            mcp_only(
                "field",
                ParamType::String,
                "Compatibility alias for a single field projection. Example: `field: \"artifacts\"`.",
            ),
            ParamSpec {
                mcp_description: Some(Description::Computed(show_run_id_description)),
                ..mcp_only("run_id", ParamType::String, "")
            },
            mcp_only(
                "workspace",
                ParamType::String,
                "Optional explicit workspace filter. `id` is resolved globally by default; do not pass cwd, MCP session/initialize metadata, or a linked-worktree runtime identity (for example `orbit-5c61b3`). When supplied, a registered workspace name, logical workspace ID (`ws_*`), or absolute local checkout path is fail-closed: a valid workspace that does not own the task returns not-found, and an unknown selector is rejected by name.",
            ),
            BOUNDED_VIEW_PARAM,
            mcp_only("limit", ParamType::Integer, "Bounded detail page size"),
            mcp_only("comments_offset", ParamType::Integer, "Comment offset"),
            mcp_only("history_offset", ParamType::Integer, "History offset"),
            mcp_only(
                "artifacts_offset",
                ParamType::Integer,
                "Artifact metadata offset",
            ),
        ],
    ],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
};

const LINT: TaskOperation = TaskOperation {
    verb: TaskVerb::Lint,
    cli_path: &["lint"],
    tool_name: "orbit.task.lint",
    tool_description: "Lint an Orbit task for stale paths, vague acceptance criteria, and stale repository names. Without `id`, sweep the active tasks (or those in `status`) for `context_files` that need repair. `restore_pruned` re-declares the selectors an earlier prune recorded in task history; without it nothing is written.",
    cli_about: "Lint tasks for context-file and acceptance-criteria problems; `--restore-pruned` re-declares recorded selectors",
    cli_help: CliHelp {
        constraints: &[],
        long_about: None,
        after_help: Some(
            "Examples:\n  orbit task lint <TASK_ID>                   # findings for one task\n  orbit task lint                             # sweep active tasks for context that needs repair\n  orbit task lint <TASK_ID> --restore-pruned  # re-declare selectors an old prune recorded\n  orbit task lint --restore-pruned            # apply the restoration across the sweep\n  orbit task lint --status review",
        ),
        flag_order: &[],
    },
    // Writes only with `restore_pruned`; the CLI opens the read-only runtime
    // without it.
    effect: Effect::Write,
    params: &[
        &[
            both(
                "id",
                ParamType::String,
                "Task ID. Omit to sweep the active tasks for `context_files` that need repair.",
                CliArgKind::Positional,
                "Task ID. Omit to sweep all active tasks for `context_files` that need repair",
            ),
            both(
                "restore_pruned",
                ParamType::Switch,
                "When true, re-declare `context_files` entries that an earlier prune recorded in task history.",
                CliArgKind::flag("restore-pruned"),
                "Re-declare `context_files` entries that an earlier prune recorded in task history",
            ),
            both(
                "status",
                ParamType::EnumList(TASK_STATUS_VALUES),
                "Sweep only tasks in these statuses (sweep mode only; refused with `id`). Pass a comma-separated string or an array.",
                CliArgKind::Flag {
                    long: "status",
                    delimiter: None,
                    aliases: &[],
                    visible_aliases: &[],
                    value_name: Some("STATUSES"),
                    multiple: false,
                },
                "Restrict the sweep to specific statuses (repeatable; sweep mode only)",
            ),
        ],
        IDENTITY_PARAMS,
    ],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: None,
};

/// The globally unique task id `show` addresses.
const TASK_ID_PARAM: ParamSpec = ParamSpec {
    required: true,
    ..both(
        "id",
        ParamType::String,
        "Globally unique task ID. Resolved through the host task registry by default; a workspace argument is not required.",
        CliArgKind::Positional,
        "Task ID",
    )
};

fn show_fields_description() -> String {
    format!(
        "Optional field projection as a string or array of strings. When set, returns only the requested field(s) as JSON. A single derived `terminal` selection remains keyed as an object. Valid values: {TASK_SHOW_PROJECTION_FIELDS_CSV}. `crew` is execution selection; `orchestrator` is separate orchestration attribution. `{TASK_SHOW_DELIVERY_FIELD}` alone returns what a delivery run committed and landed for this task instead of task fields."
    )
}

fn show_run_id_description() -> String {
    format!(
        "With `field: \"{TASK_SHOW_DELIVERY_FIELD}\"` only: the delivery run to report. Omitted, the newest task-delivery run submitted with this task is used."
    )
}
