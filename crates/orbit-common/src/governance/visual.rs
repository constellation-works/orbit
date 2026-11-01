//! The visual operations behind `orbit visual` and the `orbit.visual.*`
//! tools: the implementer's mock self-check, which renders the current
//! working tree for a task's mocks, and the operator's acceptance of a
//! disputed blocking visual review finding.
use crate::governance::operation::{
    CliArgKind, CliHelp, Effect, NounSpec, OperationSpec, ParamSpec, ParamType,
    ToolOnlyRegistration, UnknownFields, both, mcp_only, operation_registry, required,
};
use orbit_types::tool::McpToolScope;

operation_registry! {
    noun: VISUAL;
    /// The visual verbs.
    pub enum VisualVerb;
    /// Visual operations in declaration order.
    pub const VISUAL_OPERATIONS;
    {
        /// Capture the working tree beside a task's mocks.
        Capture => CAPTURE,
        /// Accept the code reviewer's dispute of a blocking visual finding.
        AcceptDispute => ACCEPT_DISPUTE,
    }
}

/// The visual noun's shared declaration.
pub const VISUAL: NounSpec<VisualVerb> = NounSpec {
    cli_name: "visual",
    tool_prefix: "orbit.visual",
    tool_only_registration: ToolOnlyRegistration::Inactive,
    cli_about: "Capture the working tree beside a task's mocks, or accept a disputed visual finding",
    audit_target_type: "task",
    audit_target_param: "id",
    cli_groups: &[],
    operations: VISUAL_OPERATIONS,
};

const MODEL: &[ParamSpec] = &[mcp_only(
    "model",
    ParamType::String,
    "Preferred provenance field. Pass the canonical agent family (codex, claude, gemini, grok, or muse); full model strings are accepted and auto-normalized.",
)];

const CAPTURE_PARAMS: &[ParamSpec] = &[
    required(both(
        "id",
        ParamType::String,
        "Task ID whose `mocks/` artifacts name the views to capture.",
        CliArgKind::Flag {
            long: "id",
            delimiter: None,
            aliases: &["task"],
            visible_aliases: &[],
            value_name: None,
            multiple: false,
        },
        "Task whose mocks name the views to capture",
    )),
    both(
        "mock",
        ParamType::String,
        "Capture only this one mock artifact (its full `mocks/…` path) instead of every mock of the task.",
        CliArgKind::flag("mock"),
        "Capture only this mock artifact instead of every mock",
    ),
    both(
        "out",
        ParamType::Path,
        "Directory the captures are written under, one subdirectory per mock. Defaults to `.orbit/tmp/visual/<task-id>` in the checkout.",
        CliArgKind::flag("out"),
        "Directory the captures are written under",
    ),
];

const CAPTURE: OperationSpec<VisualVerb> = OperationSpec {
    verb: VisualVerb::Capture,
    cli_path: &["capture"],
    tool_name: "orbit.visual.capture",
    tool_description: "Render the current working tree (uncommitted changes included) for each of a task's `mocks/` artifacts and write, per mock, `mock.png`, `capture.png` and `side-by-side.png` plus one `rules.json` with the workspace's UI rule results for those captures, under the output directory. Each mock name gives the view, state, theme and width, resolved against the view manifests configured in `visual_review.manifests`, whose fixture serves the app. This is the implementer's self-check: it attaches nothing and records nothing on the task, and its output is not evidence of record. Needs a verified browser kit on the host and a configured manifest; without either it fails naming what is missing. A mock whose view never renders is reported per mock as `render_timeout`, and one whose app's setup fails as `setup_failed`.",
    cli_about: "Capture the working tree beside each of a task's mocks",
    cli_help: CliHelp {
        long_about: Some(
            "Capture the working tree beside each of a task's mocks.\n\n\
             For every `mocks/` artifact of the task (or only `--mock`), the mock's name gives \
             the view, state, theme and width; the view manifest in `visual_review.manifests` \
             gives the route and fixture that serve it from this checkout, uncommitted changes \
             included. Writes `<out>/<mock-name>/mock.png`, `capture.png` and \
             `side-by-side.png` (mock left, capture right, common height, labelled), plus \
             `<out>/rules.json` with the workspace's UI rule results for those captures. \
             Attaches nothing and records nothing on the task.",
        ),
        after_help: Some(
            "Needs a verified browser kit (`orbit browser-kit install`) and a view manifest \
             in `visual_review.manifests`; without either the command fails naming what is \
             missing. The captures are a self-check aid, not evidence of record: a delivery \
             captures its own on the final candidate.",
        ),
        flag_order: &[],
        constraints: &[],
    },
    effect: Effect::Write,
    params: &[CAPTURE_PARAMS, MODEL],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
};

const ACCEPT_DISPUTE_PARAMS: &[ParamSpec] = &[
    required(both(
        "id",
        ParamType::String,
        "Task ID whose delivery holds on the disputed finding.",
        CliArgKind::flag("task"),
        "Task whose delivery holds on the disputed finding",
    )),
    required(both(
        "finding",
        ParamType::String,
        "The finding id in the task's visual review record (`orbit task show` lists it).",
        CliArgKind::flag("finding"),
        "Finding id in the task's visual review record",
    )),
    required(both(
        "reason",
        ParamType::String,
        "Why the dispute is accepted; recorded on the task.",
        CliArgKind::flag("reason"),
        "Why the dispute is accepted",
    )),
];

const ACCEPT_DISPUTE: OperationSpec<VisualVerb> = OperationSpec {
    verb: VisualVerb::AcceptDispute,
    cli_path: &["accept-dispute"],
    tool_name: "orbit.visual.accept_dispute",
    tool_description: "Accept the code reviewer's dispute of a confirmed visual review finding that blocks the task's delivery (`visual_review.mode = \"live\"`, at or above `visual_review.block_at`). Records the acceptance on the task; the held delivery then lands once resumed, and the finding is filed as a follow-up after landing. Operators only: managed runs are refused.",
    cli_about: "Accept a disputed blocking visual finding so the delivery can land",
    cli_help: CliHelp {
        long_about: Some(
            "Accept a disputed blocking visual finding so the delivery can land.\n\n\
             In `visual_review.mode = \"live\"`, a confirmed finding at or above \
             `visual_review.block_at` holds the candidate until the code reviewer fixes it. \
             When the reviewer disputes it instead, the delivery is held for an operator. \
             Accepting the dispute records it on the task, lets the held delivery land once \
             resumed, and files the finding as a follow-up after landing.",
        ),
        after_help: Some(
            "Only the disputed finding of a live visual review can be accepted. Managed runs \
             are refused.",
        ),
        flag_order: &[],
        constraints: &[],
    },
    effect: Effect::Write,
    params: &[ACCEPT_DISPUTE_PARAMS],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: None,
};
