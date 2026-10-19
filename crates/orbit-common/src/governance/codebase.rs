//! The read-only codebase analytics operations: the rankings the dashboard's
//! Analytics › Codebase view shows, and one path's detail behind them.
use crate::governance::operation::{
    CliArgKind, CliHelp, Effect, EnumValue, NounSpec, OperationSpec, ParamSpec, ParamType,
    Redaction, ToolOnlyRegistration, UnknownFields, both, defaulted, mcp_only, operation_registry,
    required,
};
use orbit_types::tool::McpToolScope;

operation_registry! {
    noun: CODEBASE;
    /// The codebase analytics verbs.
    pub enum CodebaseVerb;
    /// Codebase operations in declaration order.
    pub const CODEBASE_OPERATIONS;
    {
        /// Every ranking and the paths several share.
        Analytics => ANALYTICS,
        /// One path's standing in every ranking, with the tasks behind it.
        Show => SHOW,
        /// Files most often changed alongside a repository path.
        Related => RELATED,
    }
}

/// The codebase noun's shared declaration.
pub const CODEBASE: NounSpec<CodebaseVerb> = NounSpec {
    cli_name: "codebase",
    tool_prefix: "orbit.codebase",
    tool_only_registration: ToolOnlyRegistration::Inactive,
    cli_about: "Rank the repository paths that are risky for agents to change",
    audit_target_type: "codebase",
    audit_target_param: "path",
    cli_groups: &[],
    operations: CODEBASE_OPERATIONS,
};

const WINDOWS: &[EnumValue] = &[
    EnumValue::plain("7d"),
    EnumValue::plain("30d"),
    EnumValue::plain("90d"),
    EnumValue::plain("all"),
];
const ROLLUPS: &[EnumValue] = &[
    EnumValue::plain("file"),
    EnumValue::plain("dir"),
    EnumValue::plain("crate"),
];

/// The selection both verbs slice the workspace scan by.
const SELECTION: &[ParamSpec] = &[
    defaulted(
        both(
            "window",
            ParamType::Enum(WINDOWS),
            "Counted period ending now: 7d, 30d, 90d or all. Default: 90d.",
            CliArgKind::flag("window"),
            "Counted period ending now",
        ),
        "90d",
    ),
    defaulted(
        both(
            "min_support",
            ParamType::Integer,
            "Smallest denominator a ranked row needs, at least 1. Default: 15.",
            CliArgKind::flag("min-support"),
            "Smallest denominator a ranked row needs",
        ),
        "15",
    ),
    defaulted(
        both(
            "rollup",
            ParamType::Enum(ROLLUPS),
            "How paths are grouped: file, dir (parent directory) or crate. Default: file.",
            CliArgKind::flag("rollup"),
            "Group paths by file, parent directory or crate",
        ),
        "file",
    ),
];

const MODEL: &[ParamSpec] = &[mcp_only(
    "model",
    ParamType::String,
    "Preferred provenance field. Pass the canonical agent family (codex, claude, gemini, or grok); full model strings are accepted and auto-normalized.",
)];

const ANALYTICS: OperationSpec<CodebaseVerb> = OperationSpec {
    verb: CodebaseVerb::Analytics,
    cli_path: &["analytics"],
    tool_name: "orbit.codebase.analytics",
    tool_description: "Rank the workspace's repository paths by how often tasks that declared them regressed, widened their footprint onto them, were blocked by a run failure or were rejected after a run, plus lock contention and paths reserved together. Every ranking is a rate over distinct tasks with its numerator and denominator; `overlap` lists the paths found in several rankings. `task_pilot` grades each task-pilot crew and model's final selectors against the landed diffs of the tasks it assessed (recall, precision, F1, lift over the filer's selectors). The same payload the dashboard's Codebase view reads.",
    cli_about: "Show the codebase rankings and the paths several share",
    cli_help: CliHelp {
        long_about: None,
        after_help: Some(
            "\"Touched\" means a task's declared footprint, not the landed diff.\nFollow a ranked path to its tasks with `orbit codebase show <path>`.",
        ),
        flag_order: &[],
        constraints: &[],
    },
    effect: Effect::Read,
    params: &[SELECTION, MODEL],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
};

const PATH: &[ParamSpec] = &[path_param(required(both(
    "path",
    ParamType::Path,
    "A ranked key at the chosen rollup: a file path, a directory, or a crate such as crates/orbit-core/. A trailing slash is optional.",
    CliArgKind::Positional,
    "A ranked key at the chosen rollup: a file, directory or crate path",
)))];

const SHOW: OperationSpec<CodebaseVerb> = OperationSpec {
    verb: CodebaseVerb::Show,
    cli_path: &["show"],
    tool_name: "orbit.codebase.show",
    tool_description: "One repository path's standing in every codebase ranking: rate, numerator, denominator, place in the ranking, and the task IDs behind each numerator (refused requesters for lock contention, reserving requesters for each coupled pair). Use it to follow a rate from `orbit.codebase.analytics` to its tasks.",
    cli_about: "Show one path's rankings and the tasks behind each count",
    cli_help: CliHelp::NONE,
    effect: Effect::Read,
    params: &[PATH, SELECTION, MODEL],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
};

const fn path_param(mut param: ParamSpec) -> ParamSpec {
    param.redact = Redaction::Path;
    param
}

const RELATED_SELECTION: &[ParamSpec] = &[
    defaulted(
        both(
            "window",
            ParamType::Enum(WINDOWS),
            "Landed period ending now. Default: 30d.",
            CliArgKind::flag("window"),
            "Landed period ending now",
        ),
        "30d",
    ),
    defaulted(
        both(
            "min_support",
            ParamType::Integer,
            "Minimum shared landed tasks, at least 1. Default: 15.",
            CliArgKind::flag("min-support"),
            "Minimum shared landed tasks",
        ),
        "15",
    ),
    defaulted(
        both(
            "top",
            ParamType::Integer,
            "Maximum partners, from 1 to 100. Default: 10.",
            CliArgKind::flag("top"),
            "Maximum related paths (1–100)",
        ),
        "10",
    ),
];
const RELATED: OperationSpec<CodebaseVerb> = OperationSpec {
    verb: CodebaseVerb::Related,
    cli_path: &["related"],
    tool_name: "orbit.codebase.related",
    tool_description: "Rank files by landed co-change Jaccard with a repository-relative file path. Reports conditional rate, shared and individual task support, reservation coupling when present, undeclared landed partners and the tasks behind the counts. Excludes bulk tasks; missing local landed objects are reported as unavailable. Read-only; never fetches Git objects.",
    cli_about: "List files that usually change alongside a repository file",
    cli_help: CliHelp::NONE,
    effect: Effect::Read,
    params: &[RELATED_PATH, RELATED_SELECTION, MODEL],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
};

const RELATED_PATH: &[ParamSpec] = &[path_param(required(both(
    "path",
    ParamType::Path,
    "A repository-relative file path, including a deleted file with landed history.",
    CliArgKind::Positional,
    "A repository-relative file path",
)))];
