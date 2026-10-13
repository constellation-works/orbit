//! The auto-task operation registry — every auto-task verb, declared once.
//!
//! Adding a verb is one entry in the `operation_registry!` list below plus its
//! spec const; the compiler then names every surface table that owes a
//! decision about it. Consumer surfaces iterate [`AUTO_TASK`].
//!
//! The CLI and MCP shapes of `add` and `update` differ: MCP takes structured
//! `schedule` and `template` objects, while the CLI takes one flag per field.
//! Those flags are CLI-only parameters (a binding and no MCP description); the
//! CLI surface table composes them into the structured fields before the call.
//!
//! **Every string below is shipped contract.** Tool names and parameter names
//! are the MCP wire; CLI flag spellings and help text are the argv surface.

use crate::governance::operation::{
    CliArgKind, CliBinding, Description, Effect, EnumValue, NounSpec, OperationSpec, ParamSpec,
    ParamType, Redaction, operation_registry,
};
use orbit_types::tool::McpToolScope;

operation_registry! {
    noun: AUTO_TASK;
    /// Every verb the auto-task noun supports.
    ///
    /// The join between the spec table here and the handler table in
    /// `orbit-core`: a new variant fails to compile in every exhaustive
    /// surface table until the verb is wired.
    pub enum AutoTaskVerb;
    /// The auto-task operations, in `orbit auto-task --help` order. Within a
    /// spec, parameter order is the order both `--help` and the MCP tool schema
    /// list parameters.
    pub const AUTO_TASK_OPERATIONS;
    {
        /// Create a definition.
        Add => ADD,
        /// List definitions.
        List => LIST,
        /// Show one definition.
        Show => SHOW,
        /// Update a definition.
        Update => UPDATE,
        /// Enable or disable a definition.
        Toggle => TOGGLE,
        /// Delete a definition.
        Delete => DELETE,
        /// Reinstate a deleted shipped default.
        Restore => RESTORE,
        /// Mint a task from a definition now.
        Mint => MINT,
        /// Recover a stalled delivery consumer.
        Recover => RECOVER,
        /// Reset a delivery consumer.
        Reset => RESET,
    }
}

/// The auto-task noun.
pub const AUTO_TASK: NounSpec<AutoTaskVerb> = NounSpec {
    cli_name: "auto-task",
    tool_prefix: "orbit.auto_task",
    cli_about: "Define and manage recurring auto-task templates",
    audit_target_type: "auto_task",
    audit_target_param: "name",
    cli_groups: &[],
    operations: AUTO_TASK_OPERATIONS,
};

/// An auto-task operation specification.
pub type AutoTaskOperation = OperationSpec<AutoTaskVerb>;

/// `DedupePolicy` as the CLI offers it. The wire spelling is snake_case
/// (`skip_if_open`); the CLI surface table normalizes before the call.
pub const DEDUPE_VALUES: &[EnumValue] = &[
    EnumValue {
        name: "skip-if-open",
        help: Some(
            "Skip the fire while a previously-created instance is still open, so a stalled backlog never accumulates identical tasks (the default)",
        ),
        aliases: &["skip_if_open"],
    },
    EnumValue::helped(
        "always",
        "Always fire, even if a prior instance is still open",
    ),
];

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
        "Reserved top tier for the most capable (and most expensive) crews. Spelled `xhard` everywhere, so neither serde's snake_case rename nor clap's kebab-case value naming may split it into `x_hard`/`x-hard`",
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

const TOGGLE_STATE_VALUES: &[EnumValue] = &[
    EnumValue::helped("on", "Enable the definition"),
    EnumValue::helped("off", "Disable the definition (preserved, not deleted)"),
];

const DELIVERIES_LANDED_HELP: &str = "Delivery trigger JSON: branch, threshold, max_wait_minutes, coverage; optional owner_machine (defaults to this workspace's registered owner machine), max_items, retries";

const ADD: AutoTaskOperation = AutoTaskOperation {
    verb: AutoTaskVerb::Add,
    cli_path: &["add"],
    tool_name: "orbit.auto_task.add",
    tool_description: "Create a recurring auto-task definition. The generic scheduler mints a task from its template on each due slot.",
    cli_about: "Create a new auto-task definition",
    effect: Effect::Write,
    params: &[&[
        required(both(
            "name",
            ParamType::String,
            "Unique definition name (lowercase alphanumeric, `-`/`_`, starts alphanumeric). Required.",
            CliArgKind::flag("name"),
            "Unique definition name (lowercase alphanumeric, `-`/`_`, starts alphanumeric)",
        )),
        defaulted(
            free_text(both(
                "description",
                ParamType::String,
                "Human description of the recurring chore.",
                CliArgKind::flag("description"),
                "Human description of the recurring chore",
            )),
            "",
        ),
        required(mcp_only(
            "schedule",
            ParamType::Object,
            "Schedule object: exactly one of `{ cron: string }` (5-field cron) `{ every_minutes: number }`, or `{ deliveries_landed: { branch, threshold, max_wait_minutes, coverage, owner_machine?, max_items?, retries? } }` (coverage: landed_code_review_v1; owner_machine defaults to this workspace's registered owner machine). Required.",
        )),
        cli_only(
            "cron",
            ParamType::String,
            CliArgKind::flag("cron"),
            "5-field cron expression (mutually exclusive with `--every-minutes`)",
        ),
        cli_only(
            "every_minutes",
            ParamType::Integer,
            CliArgKind::flag("every-minutes"),
            "Interval in minutes (mutually exclusive with `--cron`)",
        ),
        cli_only(
            "deliveries_landed",
            ParamType::String,
            CliArgKind::flag("deliveries-landed"),
            DELIVERIES_LANDED_HELP,
        ),
        required(mcp_only(
            "template",
            ParamType::Object,
            "Task template: `{ title, description?, acceptance_criteria?, task_type?, tags?, required_tools?, priority?, complexity?, crew?, status? }`. `complexity` is an assessed low/medium/hard/xhard value copied to every minted task; omission retains legacy unassessed minting. `required_tools` is an exact canonical-name list copied to every minted task. Provider-neutral — no turn knobs. Required.",
        )),
        required(cli_only(
            "title",
            ParamType::String,
            CliArgKind::flag("title"),
            "Title of each minted task",
        )),
        defaulted(
            cli_only(
                "body",
                ParamType::String,
                CliArgKind::flag("body"),
                "Body / instruction of each minted task",
            ),
            "",
        ),
        cli_only(
            "criteria",
            ParamType::StringList,
            CliArgKind::flag("criterion"),
            "Acceptance criterion. Repeat for multiple",
        ),
        defaulted(
            cli_only(
                "task_type",
                ParamType::Enum(TASK_TYPE_VALUES),
                CliArgKind::flag("type"),
                "Task type (defaults to chore)",
            ),
            "chore",
        ),
        cli_only(
            "tags",
            ParamType::StringList,
            CliArgKind::flag("tag"),
            "Tag applied to each minted task (in addition to the provenance tag). Repeat",
        ),
        cli_only(
            "required_tools",
            ParamType::StringList,
            CliArgKind::Flag {
                long: "required-tools",
                delimiter: Some(','),
                aliases: &["required-tool"],
                visible_aliases: &[],
                value_name: None,
            },
            "Exact canonical tool names copied to every minted task",
        ),
        defaulted(
            cli_only(
                "priority",
                ParamType::Enum(TASK_PRIORITY_VALUES),
                CliArgKind::flag("priority"),
                "Priority (defaults to medium)",
            ),
            "medium",
        ),
        cli_only(
            "complexity",
            ParamType::Enum(TASK_COMPLEXITY_VALUES),
            CliArgKind::flag("complexity"),
            "Assessed complexity copied to every minted task",
        ),
        cli_only(
            "crew",
            ParamType::String,
            CliArgKind::flag("crew"),
            "Crew override for minted tasks",
        ),
        defaulted(
            cli_only(
                "status",
                ParamType::Enum(TASK_STATUS_VALUES),
                CliArgKind::flag("status"),
                "Status each minted task enters (defaults to backlog)",
            ),
            "backlog",
        ),
        defaulted(
            both(
                "dedupe",
                ParamType::Enum(DEDUPE_VALUES),
                "Dedupe policy: `skip_if_open` (default) or `always`.",
                CliArgKind::flag("dedupe"),
                "Dedupe policy (defaults to skip-if-open)",
            ),
            "skip-if-open",
        ),
    ]],
    at_least_one_of: &[],
    retired_fields: &[],
    // Managed workers can ask the owning host to edit its live definitions.
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
};

const LIST: AutoTaskOperation = AutoTaskOperation {
    verb: AutoTaskVerb::List,
    cli_path: &["list"],
    tool_name: "orbit.auto_task.list",
    tool_description: "List auto-task definitions in this workspace. Definitions seeded by a plugin that is switched off here are omitted unless include_inactive_plugins is true.",
    cli_about: "List every auto-task definition in this workspace",
    effect: Effect::Read,
    params: &[&[
        cli_only(
            "enabled",
            ParamType::Switch,
            CliArgKind::flag("enabled"),
            "Show only enabled (or, with `--disabled`, only disabled) definitions",
        ),
        cli_only(
            "disabled",
            ParamType::Switch,
            CliArgKind::flag("disabled"),
            "Show only disabled definitions",
        ),
        both(
            "include_inactive_plugins",
            ParamType::Switch,
            "Also list definitions whose seeding plugin is off in this workspace or on the host, each marked plugin_inactive with its skipped_reason.",
            CliArgKind::Flag {
                long: "include-inactive-plugins",
                delimiter: None,
                aliases: &[],
                visible_aliases: &["all"],
                value_name: None,
            },
            "Also list definitions seeded by a plugin that is switched off in this workspace or on the host, marked inactive with the reason",
        ),
        mcp_only(
            "view",
            ParamType::String,
            "bounded: bounded metadata, explicit workspace, pagination and display truncation. Omit for the existing full-record response.",
        ),
        mcp_only("offset", ParamType::Integer, "Bounded list offset"),
        mcp_only("limit", ParamType::Integer, "Bounded page size"),
    ]],
    at_least_one_of: &[],
    retired_fields: &[],
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
};

const SHOW: AutoTaskOperation = AutoTaskOperation {
    verb: AutoTaskVerb::Show,
    cli_path: &["show"],
    tool_name: "orbit.auto_task.show",
    tool_description: "Show a single auto-task definition and persisted delivery diagnostics by name.",
    cli_about: "Show a single definition by name",
    effect: Effect::Read,
    params: &[&[
        NAME_PARAM,
        both(
            "preview",
            ParamType::Switch,
            "Preview delivery baseline/source observations without admitting actions.",
            CliArgKind::flag("preview"),
            "Preview the baseline and source observations without admitting actions",
        ),
    ]],
    at_least_one_of: &[],
    retired_fields: &[],
    // `list` already returns full definitions to agents.
    mcp_scope: None,
};

const UPDATE: AutoTaskOperation = AutoTaskOperation {
    verb: AutoTaskVerb::Update,
    cli_path: &["update"],
    tool_name: "orbit.auto_task.update",
    tool_description: "Update an existing auto-task definition (present fields only), including enabling or disabling it.",
    cli_about: "Update an existing definition (present fields only)",
    effect: Effect::Write,
    params: &[&[
        mcp_only(
            "waive_batch",
            ParamType::Object,
            "Explicit settled-batch waiver: { batch_id, reason }. Cannot be combined with definition edits; advances no coverage.",
        ),
        NAME_PARAM,
        free_text(both(
            "description",
            ParamType::String,
            "New description.",
            CliArgKind::flag("description"),
            "New description",
        )),
        mcp_only(
            "schedule",
            ParamType::Object,
            "New schedule object: `{ cron: string }` `{ every_minutes: number }`, or `{ deliveries_landed: { branch, threshold, max_wait_minutes, coverage, owner_machine?, max_items?, retries? } }` (owner_machine defaults to this workspace's registered owner machine) (coverage: landed_code_review_v1).",
        ),
        cli_only(
            "cron",
            ParamType::String,
            CliArgKind::flag("cron"),
            "New 5-field cron expression (mutually exclusive with `--every-minutes`)",
        ),
        cli_only(
            "every_minutes",
            ParamType::Integer,
            CliArgKind::flag("every-minutes"),
            "New interval in minutes (mutually exclusive with `--cron`)",
        ),
        cli_only(
            "deliveries_landed",
            ParamType::String,
            CliArgKind::flag("deliveries-landed"),
            DELIVERIES_LANDED_HELP,
        ),
        mcp_only(
            "template",
            ParamType::Object,
            "Replacement task template object, including optional assessed `complexity` (low/medium/hard/xhard) and exact canonical `required_tools` copied to minted tasks.",
        ),
        both(
            "dedupe",
            ParamType::Enum(DEDUPE_VALUES),
            "New dedupe policy: `skip_if_open` or `always`.",
            CliArgKind::flag("dedupe"),
            "New dedupe policy",
        ),
        cli_only(
            "title",
            ParamType::String,
            CliArgKind::flag("title"),
            "New task title",
        ),
        cli_only(
            "body",
            ParamType::String,
            CliArgKind::flag("body"),
            "New task body",
        ),
        cli_only(
            "criteria",
            ParamType::StringList,
            CliArgKind::flag("criterion"),
            "Replace acceptance criteria. Repeat for multiple",
        ),
        cli_only(
            "task_type",
            ParamType::Enum(TASK_TYPE_VALUES),
            CliArgKind::flag("type"),
            "New task type",
        ),
        cli_only(
            "tags",
            ParamType::StringList,
            CliArgKind::flag("tag"),
            "Replace tags. Repeat for multiple",
        ),
        // One comma-separated value, where an empty value clears the list.
        preserve_empty(cli_only(
            "required_tools",
            ParamType::String,
            CliArgKind::flag("required-tools"),
            "Replacement exact canonical tools copied to minted tasks (empty clears)",
        )),
        cli_only(
            "priority",
            ParamType::Enum(TASK_PRIORITY_VALUES),
            CliArgKind::flag("priority"),
            "New priority",
        ),
        cli_only(
            "complexity",
            ParamType::Enum(TASK_COMPLEXITY_VALUES),
            CliArgKind::flag("complexity"),
            "New assessed complexity for minted tasks",
        ),
        cli_only(
            "crew",
            ParamType::String,
            CliArgKind::flag("crew"),
            "New crew override",
        ),
        cli_only(
            "status",
            ParamType::Enum(TASK_STATUS_VALUES),
            CliArgKind::flag("status"),
            "New minted-task status",
        ),
        cli_only(
            "waive_batch_id",
            ParamType::String,
            CliArgKind::Flag {
                long: "waive-batch",
                delimiter: None,
                aliases: &[],
                visible_aliases: &[],
                value_name: Some("WAIVE_BATCH"),
            },
            "Waive a settled failed delivery batch, retaining its coverage gap",
        ),
        cli_only(
            "waiver_reason",
            ParamType::String,
            CliArgKind::flag("waiver-reason"),
            "Required explanation for a batch waiver",
        ),
        mcp_only(
            "enabled",
            ParamType::Boolean,
            "Enable (`true`) or disable (`false`) the definition. Disabling is the kill-switch, not a delete.",
        ),
        mcp_only(
            "expected_enabled",
            ParamType::Boolean,
            "Observed enabled state, checked atomically with the `enabled` change; the change is refused when the definition no longer matches. Requires operator authority and `workspace`, and accepts no other edits.",
        ),
        mcp_only(
            "workspace",
            ParamType::String,
            "Explicit workspace, required with `expected_enabled`",
        ),
        // The CLI's field-scoped template edit, composed from its flags. Not
        // advertised: MCP replaces the whole `template` instead.
        ParamSpec {
            name: "template_patch",
            aliases: &[],
            param_type: ParamType::Object,
            required: false,
            preserve_empty: false,
            default: None,
            redact: Redaction::None,
            mcp_description: None,
            cli: None,
        },
    ]],
    at_least_one_of: &[],
    retired_fields: &[],
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
};

const TOGGLE: AutoTaskOperation = AutoTaskOperation {
    verb: AutoTaskVerb::Toggle,
    cli_path: &["toggle"],
    tool_name: "orbit.auto_task.toggle",
    tool_description: "Enable or disable an auto-task definition (the kill-switch; not a delete).",
    cli_about: "Enable or disable a definition (the kill-switch; not a delete)",
    effect: Effect::Write,
    params: &[&[
        NAME_PARAM,
        required(both(
            "state",
            ParamType::Enum(TOGGLE_STATE_VALUES),
            "`on` enables the definition; `off` disables it (preserved, not deleted). Required.",
            CliArgKind::Positional,
            "Whether to enable (`on`) or disable (`off`)",
        )),
    ]],
    at_least_one_of: &[],
    retired_fields: &[],
    // Agents enable or disable through `orbit.auto_task.update`'s `enabled`.
    mcp_scope: None,
};

const DELETE: AutoTaskOperation = AutoTaskOperation {
    verb: AutoTaskVerb::Delete,
    cli_path: &["delete"],
    tool_name: "orbit.auto_task.delete",
    tool_description: "Delete an auto-task definition with its scheduler cursor and delivery consumer state; a deleted shipped default stays out of later reseeds. Refused while a minted task is open unless `force` is true.",
    cli_about: "Delete a definition with its scheduler cursor and delivery consumer state; a deleted shipped default stays out of later reseeds",
    effect: Effect::Write,
    params: &[&[
        NAME_PARAM,
        both(
            "reason",
            ParamType::String,
            "Why the definition is deleted, kept in the audit record.",
            CliArgKind::flag("reason"),
            "Why the definition is deleted, kept in the audit record",
        ),
        both(
            "force",
            ParamType::Switch,
            "Delete even while a minted task is open or a delivery action is executing. The open tasks stay as they are.",
            CliArgKind::flag("force"),
            "Delete even while a minted task is open or a delivery action is executing. The open tasks stay as they are",
        ),
    ]],
    at_least_one_of: &[],
    retired_fields: &[],
    // Destructive administration stays off MCP.
    mcp_scope: None,
};

const RESTORE: AutoTaskOperation = AutoTaskOperation {
    verb: AutoTaskVerb::Restore,
    cli_path: &["restore"],
    tool_name: "orbit.auto_task.restore",
    tool_description: "Reinstate a deleted shipped auto-task default with its shipped content, disabled as shipped.",
    cli_about: "Reinstate a deleted shipped default with its shipped content",
    effect: Effect::Write,
    params: &[&[required(both(
        "name",
        ParamType::String,
        "Shipped default name. Required.",
        CliArgKind::Positional,
        "Shipped default name",
    ))]],
    at_least_one_of: &[],
    retired_fields: &[],
    mcp_scope: None,
};

const MINT: AutoTaskOperation = AutoTaskOperation {
    verb: AutoTaskVerb::Mint,
    cli_path: &["mint"],
    tool_name: "orbit.auto_task.mint",
    tool_description: "Mint one task now from an auto-task definition. Unconditional: the schedule, dedupe policy, and enabled flag are ignored, and the scheduler's own cursor is left untouched.",
    cli_about: "Mint a task from a definition now (ignores schedule, dedupe, and `enabled`; leaves the scheduler cursor untouched)",
    effect: Effect::Write,
    params: &[&[
        NAME_PARAM,
        mcp_only(
            "workspace",
            ParamType::String,
            "Explicit workspace for acknowledged mint",
        ),
        mcp_only(
            "acknowledge_unconditional",
            ParamType::Boolean,
            "True acknowledges unconditional creation; requires operator authority",
        ),
    ]],
    at_least_one_of: &[],
    retired_fields: &[],
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
};

const RECOVER: AutoTaskOperation = AutoTaskOperation {
    verb: AutoTaskVerb::Recover,
    cli_path: &["recover"],
    tool_name: "orbit.auto_task.recover",
    tool_description: "Preview or apply the audited recovery for a delivery consumer stalled by a settings change or a rebased branch. Retains all coverage debt; without `reason` it only previews.",
    cli_about: "Preview or apply the audited recovery for a delivery consumer stalled by a settings change (retains all coverage debt)",
    effect: Effect::Write,
    params: &[&[
        NAME_PARAM,
        both(
            "adopt_settings",
            ParamType::Switch,
            "Adopt the definition's current settings, retaining every pending, unresolved and accepted fact.",
            CliArgKind::flag("adopt-settings"),
            "Adopt the definition's current settings, retaining every pending, unresolved and accepted fact. Refused when the repository, branch, owner or coverage class changed, or while an action is executing",
        ),
        both(
            "reissue_action",
            ParamType::Switch,
            "Reissue the settled action that closed without accepted evidence, as a new task over its existing frozen obligations.",
            CliArgKind::flag("reissue-action"),
            "Reissue the settled action that closed without accepted evidence, as a new task over its existing frozen obligations. The closed task is left exactly as it settled",
        ),
        both(
            "replay_history",
            ParamType::Switch,
            "Prove and reconcile a content-preserving branch rebase. Cannot be combined with `adopt_settings` or `reissue_action`.",
            CliArgKind::flag("replay-history"),
            "Prove and reconcile a content-preserving branch rebase. Without `--reason` this only previews the mapping and added obligations",
        ),
        both(
            "reason",
            ParamType::String,
            "Operator explanation, retained in the recovery audit record. Required to apply.",
            CliArgKind::flag("reason"),
            "Operator explanation, retained in the recovery audit record. Required for settings adoption, action reissue, or history-replay apply",
        ),
    ]],
    at_least_one_of: &[],
    retired_fields: &[],
    mcp_scope: None,
};

const RESET: AutoTaskOperation = AutoTaskOperation {
    verb: AutoTaskVerb::Reset,
    cli_path: &["reset"],
    tool_name: "orbit.auto_task.reset",
    tool_description: "Preview or apply the audited reset of a delivery consumer: forget its coverage debt and re-baseline at the branch head. Without `reason` it only previews.",
    cli_about: "Preview or apply the audited reset of a delivery consumer: forget its coverage debt and re-baseline at the branch head",
    effect: Effect::Write,
    params: &[&[
        NAME_PARAM,
        both(
            "reason",
            ParamType::String,
            "Operator explanation, retained in the audit record. Required to apply.",
            CliArgKind::flag("reason"),
            "Operator explanation, retained in the audit record. Required to apply; without it the command previews and writes nothing",
        ),
        both(
            "force",
            ParamType::Switch,
            "Reset even while an action is executing. The admitted task or run is abandoned, not cancelled.",
            CliArgKind::flag("force"),
            "Reset even while an action is executing. The admitted task or run is abandoned, not cancelled",
        ),
    ]],
    at_least_one_of: &[],
    retired_fields: &[],
    mcp_scope: None,
};

/// The positional definition name most verbs take.
const NAME_PARAM: ParamSpec = required(both(
    "name",
    ParamType::String,
    "Definition name. Required.",
    CliArgKind::Positional,
    "Definition name",
));

/// A parameter on both MCP and the CLI.
const fn both(
    name: &'static str,
    param_type: ParamType,
    mcp_description: &'static str,
    kind: CliArgKind,
    cli_help: &'static str,
) -> ParamSpec {
    let mut param = mcp_only(name, param_type, mcp_description);
    param.cli = Some(CliBinding {
        kind,
        help: Description::Static(cli_help),
    });
    param
}

/// A parameter the tool accepts and the CLI does not bind.
const fn mcp_only(
    name: &'static str,
    param_type: ParamType,
    mcp_description: &'static str,
) -> ParamSpec {
    ParamSpec {
        name,
        aliases: &[],
        param_type,
        required: false,
        preserve_empty: false,
        default: None,
        redact: Redaction::None,
        mcp_description: Some(Description::Static(mcp_description)),
        cli: None,
    }
}

/// A CLI-only flag the CLI surface table consumes before the call.
const fn cli_only(
    name: &'static str,
    param_type: ParamType,
    kind: CliArgKind,
    help: &'static str,
) -> ParamSpec {
    ParamSpec {
        name,
        aliases: &[],
        param_type,
        required: false,
        preserve_empty: false,
        default: None,
        redact: Redaction::None,
        mcp_description: None,
        cli: Some(CliBinding {
            kind,
            help: Description::Static(help),
        }),
    }
}

const fn required(mut param: ParamSpec) -> ParamSpec {
    param.required = true;
    param
}

const fn defaulted(mut param: ParamSpec, default: &'static str) -> ParamSpec {
    param.default = Some(default);
    param
}

const fn free_text(mut param: ParamSpec) -> ParamSpec {
    param.redact = Redaction::FreeText;
    param
}

const fn preserve_empty(mut param: ParamSpec) -> ParamSpec {
    param.preserve_empty = true;
    param
}
