//! The friction operation registry — every friction verb, declared once.
//!
//! Adding a friction verb is a [`FrictionVerb`] variant, one spec const listed
//! in [`FRICTION_OPERATIONS`], and one handler arm in `orbit-core`. Consumer
//! surfaces iterate this table.
//!
//! **Every string below is shipped contract.** Tool names and parameter names
//! are the MCP wire; CLI flag spellings and help text are the argv surface.
//! Changing one is a consumer-visible break, not a rename.

use crate::governance::operation::{
    CliArgKind, CliBinding, CliRender, Description, OperationSpec, ParamSpec, ParamType,
    find_by_name,
};
use orbit_types::tool::McpToolScope;

use super::friction_tags_literal;
use super::title::FRICTION_TITLE_MAX_CHARS;

/// Every verb the friction noun supports.
///
/// This enum is the join between the spec table here and the handler table in
/// `orbit-core`: adding a variant makes both [`FrictionVerb::spec`] and the
/// runtime handler `match` fail to compile until the verb is fully wired.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FrictionVerb {
    /// Append a friction report.
    Add,
    /// List friction records.
    List,
    /// Show one friction record.
    Show,
    /// Compute friction rates.
    Stats,
    /// List configured taxonomy tags.
    Tags,
    /// Update triage metadata.
    Update,
    /// Mark a record resolved.
    Resolve,
}

/// A friction operation specification.
pub type FrictionOperation = OperationSpec<FrictionVerb>;

/// Opt-in `friction.list` response mode that returns records and guidance.
pub const FRICTION_LIST_RESPONSE_MODE_WITH_NOTES: &str = "with_notes";

impl FrictionVerb {
    /// This verb's specification.
    pub fn spec(self) -> &'static FrictionOperation {
        match self {
            FrictionVerb::Add => &ADD,
            FrictionVerb::List => &LIST,
            FrictionVerb::Show => &SHOW,
            FrictionVerb::Stats => &STATS,
            FrictionVerb::Tags => &TAGS,
            FrictionVerb::Update => &UPDATE,
            FrictionVerb::Resolve => &RESOLVE,
        }
    }

    /// The verb's short name (`add`), which is also its CLI subcommand and its
    /// audit `subcommand` label.
    pub fn as_str(self) -> &'static str {
        self.spec().name
    }

    /// The verb's fully-qualified tool name (`orbit.friction.add`).
    pub fn tool_name(self) -> &'static str {
        self.spec().tool_name
    }
}

/// The friction operation registry.
///
/// Declaration order is contract: it is the order `orbit friction --help` lists
/// subcommands. Within a spec, parameter order is the order both `--help` and
/// the MCP tool schema list parameters.
pub const FRICTION_OPERATIONS: &[FrictionOperation] =
    &[ADD, LIST, SHOW, STATS, TAGS, UPDATE, RESOLVE];

const ADD: FrictionOperation = FrictionOperation {
    verb: FrictionVerb::Add,
    name: "add",
    tool_name: "orbit.friction.add",
    tool_description: "Append a friction report",
    cli_about: "Append a friction report",
    params: &[
        ParamSpec {
            name: "body",
            param_type: ParamType::String,
            required: true,
            mcp_description: Some(BODY_HELP),
            cli: Some(CliBinding {
                kind: flag("body"),
                help: BODY_HELP,
            }),
        },
        ParamSpec {
            name: "title",
            param_type: ParamType::String,
            required: false,
            mcp_description: Some(ADD_TITLE_HELP),
            cli: Some(CliBinding {
                kind: flag("title"),
                help: ADD_TITLE_CLI_HELP,
            }),
        },
        ParamSpec {
            name: "tags",
            param_type: ParamType::StringList,
            required: false,
            mcp_description: Some(Description::Computed(add_tags_description)),
            cli: Some(CliBinding {
                kind: CliArgKind::Flag {
                    long: "tag",
                    delimiter: Some(','),
                },
                help: Description::Static(
                    "Friction taxonomy tag; repeat or comma-separate for multiple tags",
                ),
            }),
        },
        ParamSpec {
            name: "during_task",
            param_type: ParamType::String,
            required: false,
            mcp_description: Some(DURING_TASK_HELP),
            cli: Some(CliBinding {
                kind: flag("during-task"),
                help: DURING_TASK_HELP,
            }),
        },
        ParamSpec {
            name: "model",
            param_type: ParamType::String,
            required: true,
            mcp_description: Some(Description::Static(
                "Required agent family for attribution (`codex`, `claude`, `gemini`, or `grok`)",
            )),
            cli: Some(CliBinding {
                kind: flag("model"),
                help: Description::Static(
                    "Agent family to attribute the record to (`codex`, `claude`, `gemini`, or `grok`)",
                ),
            }),
        },
    ],
    rejects_agent_field: true,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
    cli_json_flag: true,
    cli_render: CliRender::Record,
};

const LIST: FrictionOperation = FrictionOperation {
    verb: FrictionVerb::List,
    name: "list",
    tool_name: "orbit.friction.list",
    tool_description: "List friction records. Returns a JSON record array by default. Set `response_mode` to `with_notes` for a stable `{records, notes}` object.",
    cli_about: "List friction records",
    params: &[
        text_param("model", "Optional model filter"),
        text_param(
            "status",
            "Optional status filter: open, triaged, or resolved",
        ),
        text_param("tag", "Optional tag filter"),
        text_param(
            "month",
            "Optional YYYY-MM month filter for reported records",
        ),
        text_param(
            "q",
            "Optional case-insensitive query over id, model, tags, status, task, and body",
        ),
        text_param("from", "Optional RFC3339 lower bound for created_at"),
        text_param("to", "Optional RFC3339 upper bound for created_at"),
        count_param("limit", "Optional maximum number of records to return"),
        count_param("offset", "Optional number of records to skip"),
        ParamSpec {
            name: "response_mode",
            param_type: ParamType::String,
            required: false,
            mcp_description: Some(Description::Static(
                "Optional response shape: `with_notes` returns `{records, notes}`; omit for the legacy record array",
            )),
            cli: None,
        },
    ],
    rejects_agent_field: false,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
    cli_json_flag: true,
    cli_render: CliRender::RecordTable,
};

const SHOW: FrictionOperation = FrictionOperation {
    verb: FrictionVerb::Show,
    name: "show",
    tool_name: "orbit.friction.show",
    tool_description: "Fetch a single friction record by id",
    cli_about: "Show a single friction record",
    params: &[BARE_ID_PARAM],
    rejects_agent_field: false,
    // `list` already returns the record bodies an agent needs; fetching one by
    // id is a human/dashboard follow-up and stays on the CLI surface.
    mcp_scope: None,
    cli_json_flag: true,
    cli_render: CliRender::Record,
};

const STATS: FrictionOperation = FrictionOperation {
    verb: FrictionVerb::Stats,
    name: "stats",
    tool_name: "orbit.friction.stats",
    tool_description: "Compute friction rates from the friction and task stores",
    cli_about: "Compute friction rates",
    params: &[],
    rejects_agent_field: false,
    // Aggregate administration stays off the MCP surface.
    mcp_scope: None,
    cli_json_flag: true,
    cli_render: CliRender::AlwaysJson,
};

const TAGS: FrictionOperation = FrictionOperation {
    verb: FrictionVerb::Tags,
    name: "tags",
    tool_name: "orbit.friction.tags",
    tool_description: "List configured friction taxonomy tags",
    cli_about: "List configured friction taxonomy tags",
    params: &[],
    rejects_agent_field: false,
    // The taxonomy is already spelled out in the `add`/`update` tag parameter
    // descriptions, so a separate advertised lookup earns nothing.
    mcp_scope: None,
    cli_json_flag: true,
    cli_render: CliRender::TagList,
};

const UPDATE: FrictionOperation = FrictionOperation {
    verb: FrictionVerb::Update,
    name: "update",
    tool_name: "orbit.friction.update",
    tool_description: "Update triage metadata for a friction record",
    cli_about: "Update triage metadata for a friction record",
    params: &[
        ParamSpec {
            name: "id",
            param_type: ParamType::String,
            required: true,
            mcp_description: Some(FRICTION_ID_HELP),
            cli: Some(CliBinding {
                kind: CliArgKind::Positional,
                help: FRICTION_ID_HELP,
            }),
        },
        text_param("status", "Optional status: open, triaged, or resolved"),
        ParamSpec {
            name: "tags",
            param_type: ParamType::StringList,
            required: false,
            mcp_description: Some(Description::Computed(update_tags_description)),
            cli: Some(CliBinding {
                kind: CliArgKind::Flag {
                    long: "tag",
                    delimiter: Some(','),
                },
                help: Description::Static(
                    "Optional replacement taxonomy tag; repeat or comma-separate for multiple tags",
                ),
            }),
        },
        text_param("body", "Optional replacement markdown body"),
        ParamSpec {
            name: "title",
            param_type: ParamType::String,
            required: false,
            mcp_description: Some(UPDATE_TITLE_HELP),
            cli: Some(CliBinding {
                kind: flag("title"),
                help: UPDATE_TITLE_CLI_HELP,
            }),
        },
    ],
    rejects_agent_field: false,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
    cli_json_flag: true,
    cli_render: CliRender::Record,
};

const RESOLVE: FrictionOperation = FrictionOperation {
    verb: FrictionVerb::Resolve,
    name: "resolve",
    tool_name: "orbit.friction.resolve",
    tool_description: "Mark a friction record as resolved",
    cli_about: "Mark a friction record as resolved",
    params: &[BARE_ID_PARAM],
    rejects_agent_field: false,
    // Resolution is an operator decision taken through the CLI / dashboard.
    mcp_scope: None,
    cli_json_flag: true,
    cli_render: CliRender::Record,
};

/// Borrow the whole registry.
pub fn friction_operations() -> &'static [FrictionOperation] {
    FRICTION_OPERATIONS
}

/// Look up a friction operation by its short verb name.
pub fn friction_operation(name: &str) -> Option<&'static FrictionOperation> {
    find_by_name(FRICTION_OPERATIONS, name)
}

const FRICTION_ID_HELP: Description = Description::Static("Friction record ID, e.g. FYYYY-MM-NNN");
const BODY_HELP: Description =
    Description::Static("Markdown body describing what happened and why it caused friction");
// The MCP descriptions carry the authoring guidance an agent needs at call
// time; the CLI help stays one line so `--help` keeps its compact layout.
const ADD_TITLE_HELP: Description = Description::Computed(add_title_description);
const ADD_TITLE_CLI_HELP: Description = Description::Computed(add_title_cli_help);
const UPDATE_TITLE_HELP: Description = Description::Computed(update_title_description);
const UPDATE_TITLE_CLI_HELP: Description = Description::Computed(update_title_cli_help);
const DURING_TASK_HELP: Description =
    Description::Static("Optional task ID being worked on when friction occurred");

/// The positional record id used by `show` and `resolve`.
///
/// Their MCP schemas describe it as a bare `friction ID` — the shape the shared
/// `orbit_id_params` helper produced before this registry existed — while the
/// CLI spells out the example. `update` declares its own `id` because its MCP
/// description was already the spelled-out form.
const BARE_ID_PARAM: ParamSpec = ParamSpec {
    name: "id",
    param_type: ParamType::String,
    required: true,
    mcp_description: Some(Description::Static("friction ID")),
    cli: Some(CliBinding {
        kind: CliArgKind::Positional,
        help: FRICTION_ID_HELP,
    }),
};

/// A `--<name> <NAME>` flag whose MCP description and CLI help agree.
const fn text_param(name: &'static str, description: &'static str) -> ParamSpec {
    ParamSpec {
        name,
        param_type: ParamType::String,
        required: false,
        mcp_description: Some(Description::Static(description)),
        cli: Some(CliBinding {
            kind: flag(name),
            help: Description::Static(description),
        }),
    }
}

/// A `--<name> <NAME>` flag carrying a non-negative integer (pagination).
const fn count_param(name: &'static str, description: &'static str) -> ParamSpec {
    ParamSpec {
        name,
        param_type: ParamType::Integer,
        required: false,
        mcp_description: Some(Description::Static(description)),
        cli: Some(CliBinding {
            kind: flag(name),
            help: Description::Static(description),
        }),
    }
}

const fn flag(long: &'static str) -> CliArgKind {
    CliArgKind::Flag {
        long,
        delimiter: None,
    }
}

fn add_title_description() -> String {
    format!(
        "One-line handle identifying the problem, at most {FRICTION_TITLE_MAX_CHARS} characters. \
         This is what lists and searches show, so name the surface and the failure. \
         Derived from the body's opening line when omitted"
    )
}

fn update_title_description() -> String {
    format!(
        "Optional replacement one-line title, at most {FRICTION_TITLE_MAX_CHARS} characters; \
         an empty string restores derivation from the body"
    )
}

fn add_title_cli_help() -> String {
    format!("One-line record handle, max {FRICTION_TITLE_MAX_CHARS} chars; derived when omitted")
}

fn update_title_cli_help() -> String {
    format!(
        "Optional replacement title, max {FRICTION_TITLE_MAX_CHARS} chars; empty restores derivation"
    )
}

fn add_tags_description() -> String {
    format!(
        "Friction taxonomy tags as a string or array; valid tags: {}; defaults to other",
        friction_tags_literal()
    )
}

fn update_tags_description() -> String {
    format!(
        "Optional replacement taxonomy tags as a string or array; valid tags: {}",
        friction_tags_literal()
    )
}
