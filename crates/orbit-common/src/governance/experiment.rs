//! Experiments: operator-defined randomized comparisons of agent treatments.
//!
//! An experiment is defined in `draft`, started, and from then on changes
//! only through events (pause, resume, reweight or arm retirement, conclude,
//! abandon); `clone` copies a definition into a new draft. Showing and
//! listing are reads any caller may make. Every write is the operator's: each
//! write tool is a governed operation that refuses agent sessions, and the
//! CLI reaches the same tools, so an agent envelope is refused there too.
use crate::governance::operation::{
    CliArgKind, CliHelp, Effect, EnumValue, NounSpec, OperationSpec, ParamSpec, ParamType,
    ToolOnlyRegistration, UnknownFields, both, free_text, mcp_only, operation_registry, required,
};
use orbit_types::tool::McpToolScope;

operation_registry! {
    noun: EXPERIMENT;
    /// The experiment verbs.
    pub enum ExperimentVerb;
    /// Experiment operations in declaration order.
    pub const EXPERIMENT_OPERATIONS;
    {
        /// Define a new draft experiment.
        Create => CREATE,
        /// Edit a draft experiment's definition.
        Update => UPDATE,
        /// Copy an experiment's definition into a new draft.
        Clone => CLONE,
        /// Start a draft experiment.
        Start => START,
        /// Pause a running experiment.
        Pause => PAUSE,
        /// Resume a paused experiment.
        Resume => RESUME,
        /// Change arm weights or retire arms, opening a new segment.
        Reweight => REWEIGHT,
        /// Force a task with no arm assignment, or an explicit_crew exclusion for the experiment, into an active arm.
        Assign => ASSIGN,
        /// Conclude an experiment with the chosen arm, or none.
        Conclude => CONCLUDE,
        /// Abandon an experiment without a conclusion.
        Abandon => ABANDON,
        /// Delete a finished experiment that never applied a treatment.
        Delete => DELETE,
        /// One experiment with its arms and event log.
        Show => SHOW,
        /// The workspace's experiments.
        List => LIST,
        /// An experiment's per-arm outcomes, intervals, health checks and verdict.
        Readout => READOUT,
    }
}

/// The experiment noun's shared declaration.
pub const EXPERIMENT: NounSpec<ExperimentVerb> = NounSpec {
    cli_name: "experiment",
    tool_prefix: "orbit.experiment",
    tool_only_registration: ToolOnlyRegistration::Inactive,
    cli_about: "Define, run and conclude experiments over agent treatments",
    audit_target_type: "experiment",
    audit_target_param: "id",
    cli_groups: &[],
    operations: EXPERIMENT_OPERATIONS,
};

const MODES: &[EnumValue] = &[
    EnumValue::helped("shadow", "Assign and record arms but apply no overlay"),
    EnumValue::helped(
        "live",
        "Start and resume are supported; overlays apply on matching dispatches. Skill replacement supports Claude on Unix only",
    ),
];

const TEMPLATES: &[EnumValue] = &[EnumValue::helped(
    "visual-review",
    "verify-mock-ui tasks, control without and treatment with the visual comparison reviewer, 1:1, live, 30 per arm, 60 days",
)];

const STATUSES: &[EnumValue] = &[
    EnumValue::plain("draft"),
    EnumValue::plain("running"),
    EnumValue::plain("paused"),
    EnumValue::plain("concluded"),
    EnumValue::plain("abandoned"),
];

const MODEL: &[ParamSpec] = &[mcp_only(
    "model",
    ParamType::String,
    "Preferred provenance field. Pass the canonical agent family (codex, claude, gemini, grok, or muse); full model strings are accepted and auto-normalized.",
)];

const ID: &[ParamSpec] = &[required(both(
    "id",
    ParamType::String,
    "The experiment's id or name, as `orbit.experiment.list` prints it.",
    CliArgKind::Positional,
    "The experiment's id or name",
))];

const ARMS_DESCRIPTION: &str = "The arms as a JSON array, each {\"name\", \"control\", \"weight\", \"overlay\"}. Exactly one arm is the control; weights are positive integers (default 1). An overlay is an object with any of: instructions (role implement, review or recovery to a repository file path, or false to suppress the shipped default), skills ({\"replace\": {skill name: bundle directory}, \"roles\"}), tools ({\"add\", \"remove\", \"roles\"}), crew ({\"pool\": [{\"crew\", \"weight\"}]}) and config ({\"values\": {key: value}, \"roles\"}). Roles default to implement. Unknown fields are refused.";

const POPULATION_DESCRIPTION: &str = "Which tasks the experiment enrolls, as a JSON object with any of: complexity, task_type, tags_include, tags_exclude and origin, each a list of strings. Empty enrolls every task.";

const GUARDRAILS_DESCRIPTION: &str = "Guardrail metrics as a JSON object from metric name to the threshold its difference from control may not pass, such as {\"delivery_done_rate\": \"-5pt\", \"escaped_regressions\": \"+0\"}.";

const METRIC_DESCRIPTION: &str = "The whole-task metric the verdict is read on: total_tokens, cost_usd, lead_time, first_pass_acceptance, review_repairs, step_recoveries, final_recoveries, failed_checks, merge_conflicts, delivery_done_rate, escaped_regressions, lines_changed, visual_landed_mismatches, visual_review_repairs or visual_verified_finding_rate. Step-local metrics are exploratory and may be used as guardrails.";

/// The definition fields `create` and `update` share, after the name.
const DEFINITION: &[ParamSpec] = &[
    free_text(both(
        "hypothesis",
        ParamType::String,
        "What the experiment expects the treatment arms to change, and why.",
        CliArgKind::flag("hypothesis"),
        "What the treatment arms are expected to change, and why",
    )),
    both(
        "arms",
        ParamType::Array,
        ARMS_DESCRIPTION,
        CliArgKind::flag("arms"),
        "The arms as a JSON array of {name, control, weight, overlay}",
    ),
    both(
        "population",
        ParamType::Object,
        POPULATION_DESCRIPTION,
        CliArgKind::flag("population"),
        "Which tasks to enroll, as a JSON object (complexity, task_type, tags_include, tags_exclude, origin)",
    ),
    both(
        "primary_metric",
        ParamType::String,
        METRIC_DESCRIPTION,
        CliArgKind::flag("primary-metric"),
        "The metric the verdict is read on",
    ),
    both(
        "guardrails",
        ParamType::Object,
        GUARDRAILS_DESCRIPTION,
        CliArgKind::flag("guardrails"),
        "Guardrail metrics and thresholds, as a JSON object",
    ),
    both(
        "min_sample",
        ParamType::Integer,
        "Tasks every arm needs before a verdict is given.",
        CliArgKind::flag("min-sample"),
        "Tasks every arm needs before a verdict",
    ),
    both(
        "max_days",
        ParamType::Integer,
        "Days after start at which a verdict is given whatever the sample.",
        CliArgKind::flag("max-days"),
        "Days after start at which a verdict is given",
    ),
    both(
        "mode",
        ParamType::Enum(MODES),
        "shadow (default) records assignments without applying overlays; live experiments can start and resume, applying overlays on matching dispatches. Start requires base-admissible instruction files and skill bundles, registered enabled agent-facing added tools, and preservation of handoff tools; skill replacement currently supports Claude on Unix.",
        CliArgKind::flag("mode"),
        "Whether overlays are applied",
    ),
];

const OPTIONAL_REASON: &[ParamSpec] = &[free_text(both(
    "reason",
    ParamType::String,
    "Why, recorded on the experiment's event.",
    CliArgKind::flag("reason"),
    "Why, recorded on the event",
))];

const REQUIRED_REASON: &[ParamSpec] = &[required(free_text(both(
    "reason",
    ParamType::String,
    "Why, recorded on the experiment's event.",
    CliArgKind::flag("reason"),
    "Why, recorded on the event",
)))];

const CREATE_NAME: &[ParamSpec] = &[
    required(both(
        "name",
        ParamType::String,
        "A name unique among this workspace's experiments.",
        CliArgKind::Positional,
        "A name unique in this workspace",
    )),
    both(
        "template",
        ParamType::Enum(TEMPLATES),
        "Start from a definition Orbit ships; any definition field given as well replaces the template's. `visual-review` compares delivering `verify-mock-ui` tasks without (control) and with (treatment) the visual comparison reviewer: 1:1, live, 30 tasks per arm, 60 days, primary metric visual_landed_mismatches. The workspace must also list `visual_review.reviewer` in `experiments.config_keys` before it starts.",
        CliArgKind::flag("template"),
        "Start from a shipped definition",
    ),
];

const CREATE: OperationSpec<ExperimentVerb> = OperationSpec {
    verb: ExperimentVerb::Create,
    cli_path: &["create"],
    tool_name: "orbit.experiment.create",
    tool_description: "Define a new experiment in draft: its arms (one control plus treatments, each with a weight and an overlay), the tasks it enrolls, its primary metric, guardrails and sample rules. Arms and overlays are validated on write and each overlay's digest is recorded. A draft assigns nothing. Operator only: an agent session is refused.",
    cli_about: "Define a new draft experiment",
    cli_help: CliHelp::NONE,
    effect: Effect::Write,
    params: &[CREATE_NAME, DEFINITION, OPTIONAL_REASON, MODEL],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
};

const RENAME: &[ParamSpec] = &[both(
    "name",
    ParamType::String,
    "A new name, unique among this workspace's experiments.",
    CliArgKind::flag("name"),
    "A new name, unique in this workspace",
)];

const UPDATE: OperationSpec<ExperimentVerb> = OperationSpec {
    verb: ExperimentVerb::Update,
    cli_path: &["update"],
    tool_name: "orbit.experiment.update",
    tool_description: "Edit a draft experiment's definition: name, hypothesis, arms (the given list replaces every arm), population, metrics and sample rules. Only a draft can be edited; a started experiment is refused and changes only through pause, resume, reweight, conclude or abandon, or is cloned into a new draft. Operator only: an agent session is refused.",
    cli_about: "Edit a draft experiment's definition",
    cli_help: CliHelp::NONE,
    effect: Effect::Write,
    params: &[ID, RENAME, DEFINITION, OPTIONAL_REASON, MODEL],
    at_least_one_of: &[
        "name",
        "hypothesis",
        "arms",
        "population",
        "primary_metric",
        "guardrails",
        "min_sample",
        "max_days",
        "mode",
    ],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
};

const CLONE_NAME: &[ParamSpec] = &[required(both(
    "name",
    ParamType::String,
    "The new draft's name, unique among this workspace's experiments.",
    CliArgKind::flag("name"),
    "The new draft's name",
))];

const CLONE: OperationSpec<ExperimentVerb> = OperationSpec {
    verb: ExperimentVerb::Clone,
    cli_path: &["clone"],
    tool_name: "orbit.experiment.clone",
    tool_description: "Copy an experiment's definition (hypothesis, arms with their current weights and overlays, population, metrics, sample rules, mode) into a new draft under a new name and a fresh assignment salt. Retired arms are not copied. The source is unchanged. Operator only: an agent session is refused.",
    cli_about: "Copy an experiment's definition into a new draft",
    cli_help: CliHelp::NONE,
    effect: Effect::Write,
    params: &[ID, CLONE_NAME, OPTIONAL_REASON, MODEL],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
};

const START: OperationSpec<ExperimentVerb> = OperationSpec {
    verb: ExperimentVerb::Start,
    cli_path: &["start"],
    tool_name: "orbit.experiment.start",
    tool_description: "Start a draft experiment, opening segment 1; from then on its definition is frozen. Refused unless experiments.enabled is true, while experiments.max_running experiments are already running, and unless the definition is complete: at least two arms with unique names, exactly one control, positive weights, a primary metric, and config overlays only for keys on experiments.config_keys. Operator only: an agent session is refused.",
    cli_about: "Start a draft experiment",
    cli_help: CliHelp::NONE,
    effect: Effect::Write,
    params: &[ID, OPTIONAL_REASON, MODEL],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
};

const PAUSE: OperationSpec<ExperimentVerb> = OperationSpec {
    verb: ExperimentVerb::Pause,
    cli_path: &["pause"],
    tool_name: "orbit.experiment.pause",
    tool_description: "Pause a running experiment: it enrolls no new tasks until resumed. Tasks already assigned keep their arm. Operator only: an agent session is refused.",
    cli_about: "Pause a running experiment",
    cli_help: CliHelp::NONE,
    effect: Effect::Write,
    params: &[ID, REQUIRED_REASON, MODEL],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
};

const RESUME: OperationSpec<ExperimentVerb> = OperationSpec {
    verb: ExperimentVerb::Resume,
    cli_path: &["resume"],
    tool_name: "orbit.experiment.resume",
    tool_description: "Resume a paused experiment. Refused while experiments.max_running other experiments are already running. Operator only: an agent session is refused.",
    cli_about: "Resume a paused experiment",
    cli_help: CliHelp::NONE,
    effect: Effect::Write,
    params: &[ID, OPTIONAL_REASON, MODEL],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
};

const REWEIGHT_PARAMS: &[ParamSpec] = &[
    both(
        "weights",
        ParamType::Object,
        "New weights as a JSON object from arm name to a positive integer. Arms not named keep their weight.",
        CliArgKind::flag("weights"),
        "New weights as a JSON object from arm name to a positive integer",
    ),
    both(
        "retire",
        ParamType::StringList,
        "Arms to retire: they take no new tasks. The control cannot be retired, and at least one treatment must remain.",
        CliArgKind::flag("retire"),
        "An arm to retire (repeatable)",
    ),
];

const REWEIGHT: OperationSpec<ExperimentVerb> = OperationSpec {
    verb: ExperimentVerb::Reweight,
    cli_path: &["reweight"],
    tool_name: "orbit.experiment.reweight",
    tool_description: "Change a running or paused experiment's arm weights, retire arms, or both. Each call opens a new segment, recorded in the event log with the weights before and after, so tasks assigned under different weights stay distinguishable. Operator only: an agent session is refused.",
    cli_about: "Change arm weights or retire arms, opening a new segment",
    cli_help: CliHelp::NONE,
    effect: Effect::Write,
    params: &[ID, REWEIGHT_PARAMS, REQUIRED_REASON, MODEL],
    at_least_one_of: &["weights", "retire"],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
};

const CONCLUDE_ARM: &[ParamSpec] = &[both(
    "arm",
    ParamType::String,
    "The arm chosen as the winner. Omit to conclude with no winner.",
    CliArgKind::flag("arm"),
    "The arm chosen as the winner (omit for none)",
)];

const CONCLUDE: OperationSpec<ExperimentVerb> = OperationSpec {
    verb: ExperimentVerb::Conclude,
    cli_path: &["conclude"],
    tool_name: "orbit.experiment.conclude",
    tool_description: "Conclude a running or paused experiment. Records the chosen arm (or none), the reason, and the readout JSON at that moment on the conclusion event, which orbit experiment show returns. A chosen arm files one proposed task naming the experiment, arm, readout verdict and overlay; no arm files nothing. Assignment stops; tasks already assigned keep their arm. Changes no configuration or repository file. Operator only: an agent session is refused.",
    cli_about: "Conclude an experiment with the chosen arm, or none",
    cli_help: CliHelp::NONE,
    effect: Effect::Write,
    params: &[ID, CONCLUDE_ARM, REQUIRED_REASON, MODEL],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
};

const ABANDON: OperationSpec<ExperimentVerb> = OperationSpec {
    verb: ExperimentVerb::Abandon,
    cli_path: &["abandon"],
    tool_name: "orbit.experiment.abandon",
    tool_description: "Abandon a draft, running or paused experiment without a conclusion. It enrolls no new tasks; tasks already assigned keep their arm. Operator only: an agent session is refused.",
    cli_about: "Abandon an experiment without a conclusion",
    cli_help: CliHelp::NONE,
    effect: Effect::Write,
    params: &[ID, REQUIRED_REASON, MODEL],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
};

const DELETE: OperationSpec<ExperimentVerb> = OperationSpec {
    verb: ExperimentVerb::Delete,
    cli_path: &["delete"],
    tool_name: "orbit.experiment.delete",
    tool_description: "Delete a draft, concluded or abandoned experiment with its arms, events and assignments. Refused for a running or paused experiment, for one that ran live and assigned tasks to arms (their crews and runs cite it), and for any experiment concluded with an arm (its promotion task names it), including shadow experiments. Shadow experiments concluded without an arm applied nothing and can be deleted, such as crew-ladder trials. Operator CLI only.",
    cli_about: "Delete a finished experiment that never applied a treatment",
    cli_help: CliHelp::NONE,
    effect: Effect::Write,
    params: &[ID, REQUIRED_REASON, MODEL],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: None,
};

const SHOW: OperationSpec<ExperimentVerb> = OperationSpec {
    verb: ExperimentVerb::Show,
    cli_path: &["show"],
    tool_name: "orbit.experiment.show",
    tool_description: "Show one experiment: its definition, status and current segment, each arm with its weight, overlay and overlay digest, and every event in order with its actor, time and reason. Read-only.",
    cli_about: "Show one experiment with its arms and event log",
    cli_help: CliHelp::NONE,
    effect: Effect::Read,
    params: &[ID, MODEL],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
};

const LIST_FILTER: &[ParamSpec] = &[both(
    "status",
    ParamType::Enum(STATUSES),
    "Only experiments in this status: draft, running, paused, concluded or abandoned.",
    CliArgKind::flag("status"),
    "Only experiments in this status",
)];

const LIST: OperationSpec<ExperimentVerb> = OperationSpec {
    verb: ExperimentVerb::List,
    cli_path: &["list"],
    tool_name: "orbit.experiment.list",
    tool_description: "List the workspace's experiments, oldest first, with status, mode, current segment and arms. Filter by status. Read-only.",
    cli_about: "List the workspace's experiments",
    cli_help: CliHelp::NONE,
    effect: Effect::Read,
    params: &[LIST_FILTER, MODEL],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
};

const ASSIGN_PARAMS: &[ParamSpec] = &[
    required(both(
        "task_id",
        ParamType::String,
        "The task to assign.",
        CliArgKind::flag("task"),
        "The task to assign",
    )),
    required(both(
        "arm",
        ParamType::String,
        "The active arm to assign.",
        CliArgKind::flag("arm"),
        "The active arm to assign",
    )),
];

const ASSIGN: OperationSpec<ExperimentVerb> = OperationSpec {
    verb: ExperimentVerb::Assign,
    cli_path: &["assign"],
    tool_name: "orbit.experiment.assign",
    tool_description: "Force a nonterminal task with no arm assignment, or with an explicit_crew exclusion for this experiment, into an active arm of a running experiment, shadow or live. Records forced=true. Existing arm assignments and exclusions other than that explicit_crew exclusion are refused; overlapping experiment surfaces are refused. Operator only: an agent session is refused.",
    cli_about: "Force a task into an active arm of a running experiment (shadow or live)",
    cli_help: CliHelp::NONE,
    effect: Effect::Write,
    params: &[ID, ASSIGN_PARAMS, MODEL],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
};

const READOUT_PARAMS: &[ParamSpec] = &[both(
    "metric",
    ParamType::String,
    "Report only this metric; the verdict is still read on the primary metric and guardrails. One of implement_tokens, total_tokens, cost_usd, lead_time, implement_duration, turns_before_first_edit, result_bytes_before_first_edit, first_pass_acceptance, review_repairs, step_recoveries, final_recoveries, failed_checks, merge_conflicts, delivery_done_rate, escaped_regressions, lines_changed, visual_landed_mismatches, visual_review_repairs or visual_verified_finding_rate.",
    CliArgKind::flag("metric"),
    "Report only this metric",
)];

const READOUT: OperationSpec<ExperimentVerb> = OperationSpec {
    verb: ExperimentVerb::Readout,
    cli_path: &["readout"],
    tool_name: "orbit.experiment.readout",
    tool_description: "Read an experiment's outcomes per arm, computed now from its assignments and the records Orbit already keeps. Intention-to-treat by task; forced assignments are left out and counted. Per arm and segment: n, mean and median with 95% bootstrap intervals (fixed seed) or Wilson intervals for rates, open (lagging) outcomes and crew mix; each arm's difference from control overall (post-stratified by complexity) and per complexity; guardrail status; a sample-ratio chi-square against the weights; overlay-digest drift. The verdict is collecting until every arm reaches min_sample or max_days pass, withheld on a sample-ratio mismatch, then winner <arm>, no difference or inconclusive. The shape carries schema_version. Read-only.",
    cli_about: "Show an experiment's per-arm outcomes, intervals and verdict",
    cli_help: CliHelp::NONE,
    effect: Effect::Read,
    params: &[ID, READOUT_PARAMS, MODEL],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
};
