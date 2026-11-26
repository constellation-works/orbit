//! The read-only codebase analytics operations: the rankings the dashboard's
//! Analytics › Codebase view shows, and one path's detail behind them.
use crate::governance::operation::{
    CliArgKind, CliHelp, Effect, EnumValue, NounSpec, OperationSpec, ParamSpec, ParamType,
    Redaction, ToolOnlyRegistration, UnknownFields, both, defaulted, operation_registry, required,
};
use orbit_types::tool::{McpIdRouteDeclaration, McpToolScope};

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
            "Period (default: 90d).",
            CliArgKind::flag("window"),
            "Counted period ending now",
        ),
        "90d",
    ),
    defaulted(
        both(
            "min_support",
            ParamType::Integer,
            "Min denominator (>=1, default: 15).",
            CliArgKind::flag("min-support"),
            "Smallest denominator a ranked row needs",
        ),
        "15",
    ),
    defaulted(
        both(
            "rollup",
            ParamType::Enum(ROLLUPS),
            "Grouping (default: file).",
            CliArgKind::flag("rollup"),
            "Group paths by file, parent directory or crate",
        ),
        "file",
    ),
];

const NATURES: &[EnumValue] = &[
    EnumValue::plain("broke_existing"),
    EnumValue::plain("defect_in_new"),
    EnumValue::plain("incomplete"),
    EnumValue::plain("merge_reintroduced"),
    EnumValue::plain("contract_drift"),
    EnumValue::plain("unclassified"),
    EnumValue::plain("absent"),
];
const ESCAPES: &[EnumValue] = &[
    EnumValue::plain("no_test"),
    EnumValue::plain("test_not_selected"),
    EnumValue::plain("platform_only"),
    EnumValue::plain("review_missed"),
    EnumValue::plain("spec_ambiguous"),
    EnumValue::plain("nondeterministic"),
    EnumValue::plain("unclassified"),
    EnumValue::plain("absent"),
];
const DETECTORS: &[EnumValue] = &[
    EnumValue::plain("delivery_review"),
    EnumValue::plain("security_review"),
    EnumValue::plain("code_review"),
    EnumValue::plain("qa_sweep"),
    EnumValue::plain("post_merge_ci"),
    EnumValue::plain("production_run"),
    EnumValue::plain("operator"),
    EnumValue::plain("other"),
];

/// Which regressions the Regressions ranking, its detail and its crew and
/// complexity rates count; omitted dimensions keep every value.
const REGRESSION_FILTER: &[ParamSpec] = &[
    both(
        "nature",
        ParamType::Enum(NATURES),
        "Filter regressions by nature (`absent`: unclassified-null).",
        CliArgKind::flag("nature"),
        "Count only regressions of this nature (absent: no classification)",
    ),
    both(
        "escape",
        ParamType::Enum(ESCAPES),
        "Filter regressions by escape (`absent`: unclassified-null).",
        CliArgKind::flag("escape"),
        "Count only regressions that escaped this way (absent: no classification)",
    ),
    both(
        "layer",
        ParamType::String,
        "Filter regressions by layer (`workflow.regression_layers`, `other`, `unknown`).",
        CliArgKind::flag("layer"),
        "Count only regressions whose fix sits in this layer (a workflow.regression_layers name, other or unknown)",
    ),
    both(
        "detected_by",
        ParamType::Enum(DETECTORS),
        "Filter regressions by detector.",
        CliArgKind::flag("detected-by"),
        "Count only regressions found this way",
    ),
];

const ANALYTICS: OperationSpec<CodebaseVerb> = OperationSpec {
    verb: CodebaseVerb::Analytics,
    cli_path: &["analytics"],
    tool_name: "orbit.codebase.analytics",
    tool_description: "Rank repository paths across codebase risk measures. Use action `show` on a ranked path to inspect its evidence. See the installed Orbit skill's `tool-surface-codebase.md` reference, Codebase analytics and path detail, for metrics, attribution, filters and response fields.",
    cli_about: "Show the codebase rankings and the paths several share",
    cli_help: CliHelp {
        long_about: None,
        after_help: Some(
            "\"Touched\" means a task's declared footprint, not the landed diff. Regressions count\nagainst the paths their links record as sites, not the whole footprint.\nFollow a ranked path to its tasks with `orbit codebase show <path>`.\nFiles absent from the checkout are marked; use --rollup dir for directory history.",
        ),
        flag_order: &[],
        constraints: &[],
    },
    effect: Effect::Read,
    params: &[SELECTION, REGRESSION_FILTER],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
    id_route: McpIdRouteDeclaration::Unspecified,
};

const PATH: &[ParamSpec] = &[path_param(required(both(
    "path",
    ParamType::Path,
    "Key at chosen rollup (file, dir, or crate e.g. `crates/orbit-core/`).",
    CliArgKind::Positional,
    "A ranked key at the chosen rollup: a file, directory or crate path",
)))];

const SHOW: OperationSpec<CodebaseVerb> = OperationSpec {
    verb: CodebaseVerb::Show,
    cli_path: &["show"],
    tool_name: "orbit.codebase.show",
    tool_description: "Inspect one repository path across codebase rankings and follow counts to tasks and sites. See the installed Orbit skill's `tool-surface-codebase.md` reference, Codebase analytics and path detail, for ranking, attribution, filters and response fields.",
    cli_about: "Show one path's rankings and the tasks behind each count",
    cli_help: CliHelp::NONE,
    effect: Effect::Read,
    params: &[PATH, SELECTION, REGRESSION_FILTER],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
    id_route: McpIdRouteDeclaration::Unspecified,
};

const RELATED_SELECTION: &[ParamSpec] = &[
    defaulted(
        both(
            "window",
            ParamType::Enum(WINDOWS),
            "Landed period (default: 30d).",
            CliArgKind::flag("window"),
            "Landed period ending now",
        ),
        "30d",
    ),
    defaulted(
        both(
            "min_support",
            ParamType::Integer,
            "Min shared tasks (>=1, default: 15).",
            CliArgKind::flag("min-support"),
            "Minimum shared landed tasks",
        ),
        "15",
    ),
    defaulted(
        both(
            "top",
            ParamType::Integer,
            "Max partners, 1-100 (default: 10).",
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
    tool_description: "Rank files by landed co-change Jaccard with a repository-relative file path (read-only). Fields and exclusions: the orbit skill's tool-surface-codebase.md reference, section `Codebase analytics and path detail`.",
    cli_about: "List files that usually change alongside a repository file",
    cli_help: CliHelp::NONE,
    effect: Effect::Read,
    params: &[RELATED_PATH, RELATED_SELECTION],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
    id_route: McpIdRouteDeclaration::Unspecified,
};

const RELATED_PATH: &[ParamSpec] = &[path_param(required(both(
    "path",
    ParamType::Path,
    "Repository-relative file path.",
    CliArgKind::Positional,
    "A repository-relative file path",
)))];

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

const fn path_param(mut param: ParamSpec) -> ParamSpec {
    param.redact = Redaction::Path;
    param
}
