//! Crew memory (dormant): per-model memory spaces and the global memory,
//! held as append-only revisions in the owner store.
//!
//! Crew memory has no operator surface. Its machinery switches on only while
//! a running experiment has a `memory` arm, and these three verbs are the
//! run-bound operations that machinery's runs call. They are never listed by
//! `orbit tool list`, served only to an MCP server whose dispatch stamp names
//! them, and cannot be granted. Core decides which calls it admits: write and
//! propose only from a crew memory dreaming task's run, with the key fixed to
//! that task's key; show from that run or a run in a memory arm, for its own
//! model's space and `global`. Every other caller, operator included, is
//! refused.
use crate::governance::operation::{
    CliHelp, Effect, NounSpec, OperationSpec, ParamSpec, ParamType, ToolOnlyRegistration,
    UnknownFields, free_text, mcp_only, operation_registry, required,
};
use orbit_types::tool::{McpIdRouteDeclaration, McpToolScope};

/// The crew memory noun's shared declaration. It has no CLI command.
pub const MEMORY: NounSpec<MemoryVerb> = NounSpec {
    cli_name: "memory",
    tool_prefix: "orbit.memory",
    tool_only_registration: ToolOnlyRegistration::Inactive,
    cli_about: "Crew memory's run-bound operations (no CLI)",
    audit_target_type: "crew_memory",
    audit_target_param: "key",
    cli_groups: &[],
    operations: MEMORY_OPERATIONS,
};

const SHOW_PARAMS: &[ParamSpec] = &[
    mcp_only(
        "key",
        ParamType::String,
        "Memory key: this run's model id or `global`. Omit for this run's own model space.",
    ),
    mcp_only(
        "revision",
        ParamType::Integer,
        "Show this revision instead of the newest. Requires key.",
    ),
];

const SHOW: OperationSpec<MemoryVerb> = OperationSpec {
    verb: MemoryVerb::Show,
    cli_path: &[],
    tool_name: "orbit.memory.show",
    tool_description: "Show a crew memory space's newest or chosen revision (read-only; dreaming or memory-arm runs only). Details: the orbit skill's tool-surface-registry.md reference, section `Workflow and crew memory tools`.",
    cli_about: "Show a memory space",
    cli_help: CliHelp::NONE,
    effect: Effect::Read,
    params: &[SHOW_PARAMS],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Refuse,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
    id_route: McpIdRouteDeclaration::Unspecified,
};

const WRITE_PARAMS: &[ParamSpec] = &[
    required(mcp_only(
        "key",
        ParamType::String,
        "Memory key: this dreaming task's own model id.",
    )),
    mcp_only(
        "files",
        ParamType::Array,
        "The space's complete new content as {path, text} objects. It replaces every file; there is no append.",
    ),
    mcp_only(
        "evidence",
        ParamType::StringList,
        "Evidence task ids supporting the notes: at least one, each from this run's evidence pack.",
    ),
    free_text(mcp_only(
        "reason",
        ParamType::String,
        "Why the space changes; stored on the revision.",
    )),
];

const WRITE: OperationSpec<MemoryVerb> = OperationSpec {
    verb: MemoryVerb::Write,
    cli_path: &[],
    tool_name: "orbit.memory.write",
    tool_description: "Replace a dreaming task's own crew memory space with a new revision (dreaming runs only). Caps and lints: the orbit skill's tool-surface-registry.md reference, section `Workflow and crew memory tools`.",
    cli_about: "Replace a memory space's files with a new revision",
    cli_help: CliHelp::NONE,
    effect: Effect::Write,
    params: &[WRITE_PARAMS],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Refuse,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
    id_route: McpIdRouteDeclaration::Unspecified,
};

const PROPOSE_PARAMS: &[ParamSpec] = &[
    required(free_text(mcp_only(
        "text",
        ParamType::String,
        "The proposed global-memory note, at most 3 KiB of Markdown. It passes the same rules as a global-memory write.",
    ))),
    required(mcp_only(
        "evidence",
        ParamType::StringList,
        "Evidence task ids the note rests on, at least one, each from this run's evidence pack.",
    )),
];

const PROPOSE: OperationSpec<MemoryVerb> = OperationSpec {
    verb: MemoryVerb::Propose,
    cli_path: &[],
    tool_name: "orbit.memory.propose",
    tool_description: "Propose a note for the global crew memory (dreaming runs only, max 2 per run). Caps and lints: the orbit skill's tool-surface-registry.md reference, section `Workflow and crew memory tools`.",
    cli_about: "Propose a global-memory note",
    cli_help: CliHelp::NONE,
    effect: Effect::Write,
    params: &[PROPOSE_PARAMS],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Refuse,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
    id_route: McpIdRouteDeclaration::Unspecified,
};

operation_registry! {
    noun: MEMORY;
    /// The crew memory verbs.
    pub enum MemoryVerb;
    /// Crew memory operations.
    pub const MEMORY_OPERATIONS;
    {
        /// One space's current or chosen revision (memory runs only).
        Show => SHOW,
        /// Replace one space's files with a new revision (dreaming runs only).
        Write => WRITE,
        /// Propose a note for the global memory (dreaming runs only).
        Propose => PROPOSE,
    }
}
