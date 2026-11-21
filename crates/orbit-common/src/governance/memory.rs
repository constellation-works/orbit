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
    tool_description: "Show a crew memory space: the files of its newest revision (or of `revision`), their digest and size, the evidence the write cited, its writer and reason. A key with no revision shows an empty space. Read only. Only a crew memory dreaming task's run or a run in a memory experiment arm may call it, and it reads only its own model's space (the default key) or `global`.",
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
    tool_description: "Replace a crew memory space's files with a new revision. A write is refused whole, naming the rule, when it breaks caps (a model space at most 8 files and 6 KiB, names [a-z0-9-]{1,40}.md), provenance (at least one evidence task id, each from the run's evidence pack), the score-blind lint (scorecard vocabulary or another crew's or model's name), the not-instructions lint (a URL, an Orbit tool name, a shell command block or an injection phrase) or rewrite-whole (each file once); a refusal is recorded in the space's history. Only a crew memory dreaming task's run may write, and only that task's own key: another model's key and `global` are refused (propose a global note instead).",
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
    tool_description: "Propose a note for the global memory every model receives. Only a crew memory dreaming task's run may propose, at most 2 proposals per run. The note is refused, naming the rule, when it breaks the global memory's caps (3 KiB), provenance (at least one evidence task id, each from the run's evidence pack), the score-blind lint or the not-instructions lint.",
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
