//! The shared lexical search operation. Reindex is local maintenance and has no tool.
use crate::governance::operation::{
    CliArgKind, CliHelp, Effect, EnumValue, NounSpec, OperationSpec, ParamType, RetiredField,
    ToolOnlyRegistration, UnknownFields, aliased, both, defaulted, mcp_only, operation_registry,
    preserve_empty,
};
use orbit_types::tool::McpToolScope;

operation_registry! {
    noun: SEARCH;
    /// The shared search verbs.
    pub enum SearchVerb;
    /// Search operations in declaration order.
    pub const SEARCH_OPERATIONS;
    {
        /// Lexical task and friction query (including tool-only friction listings).
        Query => QUERY,
    }
}

const ABOUT: &str = "Search tasks and frictions";

/// Search's shared declaration; its CLI binds the query at the noun root.
pub const SEARCH: NounSpec<SearchVerb> = NounSpec {
    cli_name: "search",
    tool_prefix: "orbit.search",
    tool_only_registration: ToolOnlyRegistration::Inactive,
    cli_about: ABOUT,
    audit_target_type: "search",
    audit_target_param: "",
    cli_groups: &[],
    operations: SEARCH_OPERATIONS,
};
const KINDS: &[EnumValue] = &[
    EnumValue::plain("task"),
    EnumValue::plain("friction"),
    EnumValue::plain("all"),
];

const QUERY: OperationSpec<SearchVerb> = OperationSpec {
    verb: SearchVerb::Query,
    cli_path: &["query"],
    tool_name: "orbit.search",
    tool_description: "Search tasks and frictions using lexical matching. With `kind: friction` and no `query`, list friction records instead: every status unless a `friction:` status narrows it, filtered by `tag`, ordered by creation time then ID, each hit carrying the full `record`; a note reports a listing cut at `limit`.",
    cli_about: ABOUT,
    cli_help: CliHelp {
        long_about: None,
        after_help: Some("Forms:\n  orbit search <query>\n  orbit search reindex"),
        flag_order: &[],
        constraints: &[],
    },
    effect: Effect::Read,
    params: &[&[
        preserve_empty(both(
            "query",
            ParamType::String,
            "Tasks match non-adjacent terms: full matches first, then partial matches by term count and BM25. Frictions match a case-insensitive substring. Optional with `kind: friction`, where omitting it lists frictions.",
            CliArgKind::Positional,
            "Tasks match non-adjacent terms: all terms first, then partial matches by term count and BM25. Frictions match a case-insensitive substring",
        )),
        defaulted(
            both(
                "kind",
                ParamType::Enum(KINDS),
                "Corpus kind: task, friction, or all. Default: all.",
                flag("kind"),
                "Restrict results to one corpus kind",
            ),
            "all",
        ),
        defaulted(
            both(
                "limit",
                ParamType::Integer,
                "Maximum number of results, at most 1000. Default: 10, or 1000 for a friction listing.",
                flag("limit"),
                "Maximum total results returned (across all kinds when --kind all; round-robin per kind to ensure fair representation)",
            ),
            "10",
        ),
        aliased(
            both(
                "tag",
                ParamType::StringList,
                "AND-filter by tag. Repeat or pass an array. Applies to task and friction.",
                list_flag("tag", Some("TAGS")),
                "Filter by tag (AND semantics). Applies to task and friction results",
            ),
            &["tags"],
        ),
        both(
            "all",
            ParamType::Switch,
            "Include every status for the queried kind. Task defaults to proposed/backlog/in-progress/review/blocked; all adds done/rejected/archived/someday. Friction query defaults to open; all adds triaged/resolved.",
            flag("all"),
            "Include every status for the queried kind. Task defaults to proposed/backlog/in-progress/review/blocked; --all adds done/rejected/archived/someday. Friction defaults to open; --all adds triaged/resolved",
        ),
        aliased(
            both(
                "status",
                ParamType::StringList,
                "Explicit per-kind status override using kind:value tokens, such as task:open or friction:open. task:open covers proposed/backlog/in-progress/review/blocked. Overrides `all` for the named kind.",
                list_flag("status", None),
                "Explicit per-kind status override, e.g. task:open,friction:open. task:open covers proposed/backlog/in-progress/review/blocked. Overrides --all for the named kind",
            ),
            &["statuses"],
        ),
        aliased(
            both(
                "workspaces",
                ParamType::StringList,
                "Federated scope: registered workspace names, `ws_*` IDs, or absolute checkout paths to search together. Hits carry workspace attribution. Omit to search only the bound workspace.",
                list_flag("workspaces", Some("SELECTOR")),
                "Search this registered workspace as well. Repeat or comma-separate to federate across several; every hit is labelled with the workspace it came from. Accepts a registered name, a `ws_*` ID, or an absolute checkout path. Distinct from the top-level `orbit --workspace`, which binds the whole invocation to one checkout",
            ),
            &["workspace_scope"],
        ),
        aliased(
            both(
                "all_workspaces",
                ParamType::Switch,
                "Search every active workspace registered on the answering machine. Overrides `workspaces`. Not available inside an Orbit-managed run.",
                flag("all-workspaces"),
                "Search every active workspace registered on this machine. Overrides `--workspaces`",
            ),
            &["allWorkspaces"],
        ),
        mcp_only(
            "model",
            ParamType::String,
            "Preferred provenance field. Pass the canonical agent family (codex, claude, gemini, or grok); full model strings are accepted and auto-normalized.",
        ),
    ]],
    at_least_one_of: &[],
    retired_fields: &[
        RetiredField::verbatim(
            "related",
            "unknown parameter `related`; search supports lexical queries only",
        ),
        RetiredField::verbatim(
            "path",
            "unknown parameter `path`; search supports lexical queries only",
        ),
        RetiredField::verbatim(
            "hybrid",
            "unknown parameter `hybrid`; search supports lexical queries only",
        ),
        RetiredField::verbatim(
            "semantic",
            "unknown parameter `semantic`; search supports lexical queries only",
        ),
        RetiredField::verbatim(
            "id",
            "unknown parameter `id`; search supports lexical queries only",
        ),
        RetiredField::verbatim(
            "task_id",
            "unknown parameter `task_id`; search supports lexical queries only",
        ),
        RetiredField::verbatim(
            "taskId",
            "unknown parameter `taskId`; search supports lexical queries only",
        ),
        RetiredField::verbatim(
            "field",
            "unknown parameter `field`; search supports lexical queries only",
        ),
        RetiredField::verbatim(
            "embedding_model",
            "unknown parameter `embedding_model`; search supports lexical queries only",
        ),
        RetiredField::verbatim(
            "embeddingModel",
            "unknown parameter `embeddingModel`; search supports lexical queries only",
        ),
        RetiredField::verbatim(
            "embedding-model",
            "unknown parameter `embedding-model`; search supports lexical queries only",
        ),
        RetiredField::verbatim(
            "semantic_model",
            "unknown parameter `semantic_model`; search supports lexical queries only",
        ),
        RetiredField::verbatim(
            "semanticModel",
            "unknown parameter `semanticModel`; search supports lexical queries only",
        ),
    ],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
};

const fn flag(long: &'static str) -> CliArgKind {
    CliArgKind::flag(long)
}
const fn list_flag(long: &'static str, value_name: Option<&'static str>) -> CliArgKind {
    CliArgKind::Flag {
        long,
        delimiter: Some(','),
        aliases: &[],
        visible_aliases: &[],
        value_name,
        multiple: false,
    }
}

/// Preserve the search handler's legacy string booleans before generic type validation.
pub fn normalize_legacy_input(input: &mut serde_json::Value) {
    for name in ["all", "all_workspaces", "allWorkspaces"] {
        let value = input
            .get(name)
            .and_then(serde_json::Value::as_str)
            .and_then(|raw| match raw.trim().to_ascii_lowercase().as_str() {
                "true" => Some(true),
                "false" => Some(false),
                _ => None,
            });
        if let Some(value) = value {
            input[name] = serde_json::Value::Bool(value);
        }
    }
}
