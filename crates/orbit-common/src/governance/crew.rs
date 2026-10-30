//! The read-only crew operations behind `orbit crew` and the `orbit.crew.*`
//! tools: the crew ladder report.
use crate::governance::operation::{
    CliHelp, Effect, NounSpec, OperationSpec, ParamSpec, ParamType, ToolOnlyRegistration,
    UnknownFields, mcp_only, operation_registry,
};
use orbit_types::tool::McpToolScope;

operation_registry! {
    noun: CREW;
    /// The crew verbs.
    pub enum CrewVerb;
    /// Crew operations in declaration order.
    pub const CREW_OPERATIONS;
    {
        /// The crew ladder: per tier, pool, floor, candidate and trial.
        Ladder => LADDER,
    }
}

/// The crew noun's shared declaration.
pub const CREW: NounSpec<CrewVerb> = NounSpec {
    cli_name: "crew",
    tool_prefix: "orbit.crew",
    tool_only_registration: ToolOnlyRegistration::Inactive,
    cli_about: "Show how crews are laddered across complexity tiers",
    audit_target_type: "crew",
    audit_target_param: "tier",
    cli_groups: &[],
    operations: CREW_OPERATIONS,
};

const MODEL: &[ParamSpec] = &[mcp_only(
    "model",
    ParamType::String,
    "Preferred provenance field. Pass the canonical agent family (codex, claude, gemini, grok, or muse); full model strings are accepted and auto-normalized.",
)];

const LADDER: OperationSpec<CrewVerb> = OperationSpec {
    verb: CrewVerb::Ladder,
    cli_path: &["ladder"],
    tool_name: "orbit.crew.ladder",
    tool_description: "The workspace's crew ladder (workflow.crew_ladder): for each complexity tier, its configured pool, its floor (the cheapest ladder rung in that pool), the candidate rung directly below the floor, and the standing shadow trial for that candidate with its would-be assignment count, or the reason the tier has no trial (mode off, tier not listed, no ladder rung in the pool, floor already the bottom rung, a running operator crew experiment, experiments disabled, experiments.max_running reached, or pending until the next delivery admission). Read-only: trials are created, started and concluded at delivery admission.",
    cli_about: "Show each tier's pool, floor, candidate rung and shadow trial",
    cli_help: CliHelp {
        long_about: None,
        after_help: Some(
            "The floor is a tier's cheapest rung of workflow.crew_ladder.rungs among the enabled,\nweighted members of workflow.<tier>_complexity_crews; the candidate is the rung directly\nbelow it. With workflow.crew_ladder.mode = \"shadow\", each listed tier with a candidate\nkeeps one shadow experiment owned by system:crew-ladder: its tasks are assigned to the\ncontrol or candidate arm and recorded, and no task's crew changes. WOULD-BE counts the\ntasks assigned to the candidate arm. Trials are created, started and concluded at\ndelivery admission; this command only reads them.",
        ),
        flag_order: &[],
        constraints: &[],
    },
    effect: Effect::Read,
    params: &[MODEL],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
};
