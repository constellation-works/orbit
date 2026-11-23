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

use orbit_types::task::TASK_SHOW_DELIVERY_FIELD;
use orbit_types::tool::{McpIdRouteDeclaration, McpToolScope};

use super::lifecycle_operations::{
    BLOCK_TRIAGE, DELETE, RECONCILE_REVIEW, RECOVER_CLAIM, REJECT, REPILOT, REQUEUE,
    RESUME_DELIVERY, RETRY_HANDOFF, REVIEW_RESET, REVOKE_HANDOFF,
};
use super::nested_operations::{
    ARTIFACT_GET, ARTIFACT_PUT, LOCKS_LIST, LOCKS_RELEASE, LOCKS_RESERVE,
};
use super::write_operations::{ADD, UPDATE};
use crate::OrbitError;
use crate::governance::operation::{
    CliArgKind, CliGroup, CliHelp, Description, Effect, EnumValue, NounSpec, OperationSpec,
    ParamSpec, ParamType, RETIRED_AGENT_FIELD, ToolOnlyRegistration, UnknownFields, aliased, both,
    cli_only, defaulted, mcp_only, operation_registry, preserve_empty,
};
use crate::protocol::tool_input::required_string;
use serde_json::{Map, Value};

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
        /// Continue a blocked delivery in a bounded recovery leaf.
        ResumeDelivery => RESUME_DELIVERY,
        /// Explicitly request one new task-pilot assessment.
        Repilot => REPILOT,
        /// Read escalated blocks awaiting triage, or record one triage
        /// decision within the shared final-recovery requeue bound.
        BlockTriage => BLOCK_TRIAGE,
        /// Retry a stopped or uncertain authorized owner handoff.
        RetryHandoff => RETRY_HANDOFF,
        /// Recover a live claim to a lifecycle-valid status, as an operator decision.
        RecoverClaim => RECOVER_CLAIM,
        /// Revoke a handoff's completion authority, as an operator decision.
        RevokeHandoff => REVOKE_HANDOFF,
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
    EnumValue::helped("done", "Accepted and closed (terminal)"),
    EnumValue::helped(
        "blocked",
        "Temporarily paused (waiting on a dependency or decision)",
    ),
    EnumValue::helped("archived", "Soft-deleted (terminal)"),
    EnumValue::helped("rejected", "Declined"),
    EnumValue::helped("someday", "Future-scoped — wanted but not yet actionable"),
];

/// The statuses a task can be eligible from: work nobody has taken yet.
pub(crate) const ELIGIBLE_STATUS_VALUES: &[EnumValue] =
    &[EnumValue::plain("backlog"), EnumValue::plain("proposed")];

/// `--help` for the `--host` flag every id-routed task command takes.
pub const TASK_HOST_HELP: &str = "Host that `--workspace` names (`orbit host list` name or `machine_id`); without it a task id routes by its prefix";

/// The `view: "bounded"` selector of `orbit.task.show`.
const BOUNDED_VIEW_PARAM: ParamSpec = mcp_only(
    "view",
    ParamType::String,
    "Bounded view; requires `workspace`, rejects `fields`, `field`, `run_id`, `snapshot`. The only accepted value is `bounded`.",
);

const LIST: TaskOperation = TaskOperation {
    verb: TaskVerb::List,
    cli_path: &["list"],
    tool_name: "orbit.task.list",
    tool_description: "List tasks as {tasks, total, truncated}; rules are in the installed Orbit skill `references/task-fields-projections.md`.",
    cli_about: "List tasks with optional filters",
    cli_help: CliHelp {
        constraints: &[],
        long_about: None,
        after_help: Some(
            "Status meanings, lifecycle and listing-order rules: installed Orbit skill `references/task-fields-projections.md`.\n\nExamples:\n  orbit task list\n  orbit task list --limit 100\n  orbit task list --status backlog\n  orbit task list --status in-progress,review\n  orbit task list --type feature\n  orbit task list --origin operator_request\n  orbit task list --priority high\n  orbit task list --parent T12345678-123456\n  orbit task list --ref jira:ENG-1234\n  orbit task list --has-ref jira\n  orbit task list --tag perf --tag bench\n  orbit task list --path src/auth/login.rs\n  orbit task list --json",
        ),
        flag_order: &[
            "status",
            "scheduled",
            "not_before_from",
            "not_before_until",
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
    params: &[&[
        both(
            "status",
            ParamType::EnumList(TASK_STATUS_VALUES),
            "Statuses.",
            CliArgKind::Flag {
                long: "status",
                delimiter: Some(','),
                aliases: &[],
                visible_aliases: &[],
                value_name: None,
                multiple: false,
            },
            "Filter by one or more statuses (comma-separated)",
        ),
        aliased(
            both(
                "parent_id",
                ParamType::String,
                "Parent ID.",
                CliArgKind::flag("parent"),
                "Filter to subtasks belonging to a parent task",
            ),
            &["parent", "parentId"],
        ),
        aliased(
            both(
                "type",
                ParamType::Enum(TASK_TYPE_VALUES),
                "Task type.",
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
            "scheduled",
            ParamType::Switch,
            "Future schedules, soonest first.",
            CliArgKind::flag("scheduled"),
            "List future-scheduled tasks soonest first, including proposed tasks labelled by status",
        ),
        preserve_empty(both(
            "not_before_from",
            ParamType::String,
            "Min `not_before` (RFC 3339).",
            CliArgKind::flag("not-before-from"),
            "Inclusive lower schedule bound (RFC 3339 with an offset)",
        )),
        preserve_empty(both(
            "not_before_until",
            ParamType::String,
            "Max `not_before` (RFC 3339).",
            CliArgKind::flag("not-before-until"),
            "Inclusive upper schedule bound (RFC 3339 with an offset)",
        )),
        both(
            "origin",
            ParamType::Enum(TASK_ORIGIN_VALUES),
            "Origin.",
            CliArgKind::flag("origin"),
            "Filter by task origin (operator_request, orchestrator_initiative, automation); tasks without a recorded origin never match",
        ),
        both(
            "job_run_id",
            ParamType::String,
            "Job run ID.",
            CliArgKind::flag("job-run-id"),
            "Filter by job run ID",
        ),
        aliased(
            both(
                "tag",
                ParamType::StringList,
                "All tags must match.",
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
            "Host OS.",
            CliArgKind::flag("runs-on"),
            "Filter by effective host OS: tags intersected with the stored crew executor; unrestricted tasks match, unresolved crews and conflicts do not",
        ),
        both(
            "pilot_warnings",
            ParamType::Switch,
            "Only pilot-warned proposed.",
            CliArgKind::flag("pilot-warnings"),
            "Keep proposed tasks held by pilot warning findings",
        ),
        both(
            "ready",
            ParamType::Switch,
            "Only satisfied dependencies.",
            CliArgKind::flag("ready"),
            "Keep only tasks whose dependencies are already satisfied",
        ),
        both(
            "path",
            ParamType::String,
            "Matching context path.",
            CliArgKind::flag("path"),
            "Filter to tasks whose `context_files` selectors apply to this path",
        ),
        defaulted(
            both(
                "limit",
                ParamType::Integer,
                "Max tasks (default 50).",
                CliArgKind::flag("limit"),
                "Maximum number of tasks to return (default 50). Must be at least 1",
            ),
            "50",
        ),
        mcp_only(
            "fields",
            ParamType::StringList,
            "Name fields, or fetch one task with `orbit.task.show`; compact rows match `orbit tool run orbit.task.list`.",
        ),
        mcp_only(
            "view",
            ParamType::String,
            "Bounded view; requires `workspace`, rejects `fields` and non-bounded filters (`status`, `priority`, `search`, `offset`, `limit` only). Only value: `bounded`.",
        ),
        mcp_only("offset", ParamType::Integer, "Bounded offset."),
        mcp_only("search", ParamType::String, "Bounded key/title filter."),
        both(
            "priority",
            ParamType::Enum(TASK_PRIORITY_VALUES),
            "Priority.",
            CliArgKind::flag("priority"),
            "Filter by priority level",
        ),
        both(
            "external_ref",
            ParamType::String,
            "External ref (`system:id`).",
            CliArgKind::flag("ref"),
            "Filter by exact external reference in `system:id` form",
        ),
        both(
            "has_ref",
            ParamType::String,
            "Has ref in system.",
            CliArgKind::flag("has-ref"),
            "Filter by external reference system",
        ),
        cli_only(
            "all",
            ParamType::Switch,
            CliArgKind::flag("all"),
            "Deprecated no-op; accepted and ignored",
        ),
        cli_only(
            "ops",
            ParamType::Switch,
            CliArgKind::flag("ops"),
            "Output signal-tier JSON (id, parent_id, title, type, status, priority, complexity)",
        ),
        cli_only(
            "full",
            ParamType::Switch,
            CliArgKind::flag("full"),
            "Show all table columns in text output",
        ),
    ]],
    at_least_one_of: &[],
    retired_fields: &[RETIRED_AGENT_FIELD],
    unknown_fields: UnknownFields::Refuse,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
    id_route: McpIdRouteDeclaration::Unspecified,
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
    params: &[&[
        both(
            "status",
            ParamType::EnumList(ELIGIBLE_STATUS_VALUES),
            "Statuses (default: both).",
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
            "Match `context_files` path.",
            CliArgKind::flag("path"),
            "Keep only candidates whose `context_files` selectors apply to this path, matched as `orbit task list --path` matches",
        ),
        defaulted(
            both(
                "limit",
                ParamType::Integer,
                "Max tasks (default 50, >=1).",
                CliArgKind::flag("limit"),
                "Maximum eligible tasks to list (default 50). Must be at least 1",
            ),
            "50",
        ),
        both(
            "explain",
            ParamType::Switch,
            "Also return `conflicting`.",
            CliArgKind::flag("explain"),
            "Also list the held-back candidates, each with the overlapping file and the in-progress or review task holding it",
        ),
    ]],
    at_least_one_of: &[],
    retired_fields: &[RETIRED_AGENT_FIELD],
    unknown_fields: UnknownFields::Refuse,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
    id_route: McpIdRouteDeclaration::Unspecified,
};

const SHOW: TaskOperation = TaskOperation {
    verb: TaskVerb::Show,
    cli_path: &["show"],
    tool_name: "orbit.task.show",
    tool_description: "Fetch a task as JSON by globally unique `id`; optional `workspace` is a fail-closed filter. Use `fields` (alias `field`) for projections, or `field: \"delivery\"` for bounded commit/landing evidence. Projection, routing and delivery details: installed Orbit skill `references/task-fields-projections.md`.",
    cli_about: "Show detailed information about a task, found by ID with an optional workspace filter",
    cli_help: CliHelp {
        long_about: Some(
            "Show a task by ID. The `Runs on` readout summarizes the task's effective OS requirement by intersecting its OS tags with the stored crew executor's `host_os`. It identifies the source of the requirement or any unresolved or conflicting constraint. This readout is derived and never stored. Without a crew, automatic routing remains undecided.",
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
                "Bounded versioned snapshot; requires `workspace`, rejects `run_id`, `view`, offsets; snapshot grants no authority.",
            ),
        ],
        &[
            ParamSpec {
                name: "fields",
                aliases: &[],
                param_type: ParamType::StringList,
                required: false,
                preserve_empty: false,
                default: None,
                redact: crate::governance::operation::Redaction::None,
                mcp_description: Some(Description::Static(
                    "Keyed response fields (`delivery`: commit/landing evidence). Rejects `snapshot`, `view`, pagination.",
                )),
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
                    help: Description::Static(
                        "Print only the specified field projection(s), for example `--fields title,status`. Repeat the flag or use a comma-separated list. Valid names and JSON output shapes: installed Orbit skill `references/task-fields-projections.md`. No pagination with a projection on a claimed read: drop the projection to page the full task, or drop pagination for a projection.",
                    ),
                }),
            },
            mcp_only(
                "field",
                ParamType::String,
                "Single-field alias for `fields` (e.g. `\"artifacts\"`). Rejects `snapshot`, `view`, pagination.",
            ),
            ParamSpec {
                mcp_description: Some(Description::Computed(show_run_id_description)),
                ..mcp_only("run_id", ParamType::String, "")
            },
            mcp_only(
                "workspace",
                ParamType::String,
                "Optional workspace filter by registered name, ID or path; task `id` is resolved globally by default.",
            ),
            BOUNDED_VIEW_PARAM,
            mcp_only(
                "limit",
                ParamType::Integer,
                "Claimed read page size; not with `snapshot` or projections.",
            ),
            mcp_only(
                "comments_offset",
                ParamType::Integer,
                "Comment page offset; not with `snapshot` or projections.",
            ),
            mcp_only(
                "history_offset",
                ParamType::Integer,
                "History page offset; not with `snapshot` or projections.",
            ),
            mcp_only(
                "artifacts_offset",
                ParamType::Integer,
                "Artifact page offset; not with `snapshot` or projections.",
            ),
        ],
    ],
    at_least_one_of: &[],
    retired_fields: &[RETIRED_AGENT_FIELD],
    unknown_fields: UnknownFields::Refuse,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
    id_route: McpIdRouteDeclaration::Task,
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
            "Status meanings and lifecycle rules: installed Orbit skill `references/task-fields-projections.md`.\n\nExamples:\n  orbit task lint <TASK_ID>                   # findings for one task\n  orbit task lint                             # sweep active tasks for context that needs repair\n  orbit task lint <TASK_ID> --restore-pruned  # re-declare selectors an old prune recorded\n  orbit task lint --restore-pruned            # apply the restoration across the sweep\n  orbit task lint --status review",
        ),
        flag_order: &[],
    },
    // Writes only with `restore_pruned`; the CLI opens the read-only runtime
    // without it.
    effect: Effect::Write,
    params: &[&[
        aliased_task_id(both(
            "id",
            ParamType::String,
            "Task ID. Omit to sweep the active tasks for `context_files` that need repair.",
            CliArgKind::Positional,
            "Task ID. Omit to sweep all active tasks for `context_files` that need repair",
        )),
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
    ]],
    at_least_one_of: &[],
    retired_fields: &[RETIRED_AGENT_FIELD],
    unknown_fields: UnknownFields::Refuse,
    mcp_scope: None,
    id_route: McpIdRouteDeclaration::Unspecified,
};

/// Unadvertised wire alias for `id` on id-keyed task tools.
///
/// ORB-26989: `#8734` removed deprecated aliases deliberately, and Daniel made
/// this one an exception because `task_id` without `id` caused 34% of
/// task-tool refusals while costing 0 schema tokens (`ParamSpec::aliases` is
/// omitted from every advertised schema).
pub const TASK_ID_WIRE_ALIAS: &str = "task_id";

const TASK_ID_ALIASES: &[&str] = &[TASK_ID_WIRE_ALIAS];

/// Attach the unadvertised `task_id` wire alias to a task operation's `id`
/// parameter (ORB-26989 exception to `#8734`).
pub(crate) const fn aliased_task_id(param: ParamSpec) -> ParamSpec {
    aliased(param, TASK_ID_ALIASES)
}

/// Refusal when a caller sets both `id` and its `task_id` alias.
pub fn task_id_alias_conflict_error() -> OrbitError {
    OrbitError::InvalidInput(format!(
        "`{TASK_ID_WIRE_ALIAS}` is an alias of `id`; set one or the other, not both"
    ))
}

/// Rename `task_id` to `id` on a JSON object when `id` is absent, refusing
/// when both keys are set.
///
/// Used by pre-spec handlers (guarded writes, preloaded artifact puts) that
/// run before `OperationSpec::parse_input_fields`. See [`TASK_ID_WIRE_ALIAS`]
/// for why this ORB-26989 exception exists after `#8734`.
pub fn normalize_task_id_alias(object: &mut Map<String, Value>) -> Result<(), OrbitError> {
    let Some(task_id) = object.remove(TASK_ID_WIRE_ALIAS) else {
        return Ok(());
    };
    if object.contains_key("id") {
        return Err(task_id_alias_conflict_error());
    }
    object.insert("id".to_string(), task_id);
    Ok(())
}

/// Read `id` from `input`, falling back to the unadvertised `task_id` alias
/// when `id` is absent, and refusing when both are set.
///
/// See [`TASK_ID_WIRE_ALIAS`] for why this ORB-26989 exception exists after
/// `#8734`.
pub fn resolve_task_id_alias(input: &Value) -> Result<String, OrbitError> {
    if input.get("id").is_some() && input.get(TASK_ID_WIRE_ALIAS).is_some() {
        return Err(task_id_alias_conflict_error());
    }
    required_string(input, &["id", TASK_ID_WIRE_ALIAS], "id")
}

/// Borrow `id`, or `task_id` when `id` is absent, as a non-empty trimmed string
/// for pre-dispatch routing; returns `None` when both are set or neither is a
/// non-empty string.
///
/// See [`TASK_ID_WIRE_ALIAS`] for why this ORB-26989 exception exists after
/// `#8734`.
pub fn task_id_alias_str(input: &Value) -> Option<&str> {
    if input.get("id").is_some() && input.get(TASK_ID_WIRE_ALIAS).is_some() {
        return None;
    }
    input
        .get("id")
        .or_else(|| input.get(TASK_ID_WIRE_ALIAS))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
}

/// The globally unique task id `show` addresses.
const TASK_ID_PARAM: ParamSpec = ParamSpec {
    required: true,
    ..aliased_task_id(both(
        "id",
        ParamType::String,
        "Globally unique task ID.",
        CliArgKind::Positional,
        "Task ID",
    ))
};

fn show_run_id_description() -> String {
    format!(
        "Delivery run for `field: \"{TASK_SHOW_DELIVERY_FIELD}\"` (default: newest). Rejects `snapshot`, `view`, pagination."
    )
}
