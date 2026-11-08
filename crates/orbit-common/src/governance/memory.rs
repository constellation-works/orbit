//! Crew memory (experimental): per-model memory spaces and the global
//! memory, held as append-only revisions in the owner store.
//!
//! History, export, reset and copy are the operator's: they have no MCP
//! scope and register inactive, and export, reset and copy are governed
//! operator-only operations, so an agent reaching them through
//! `orbit tool run` is refused at the capability chokepoint. Show, write and
//! propose (slice M3) are also agent tools: core admits an agent's call only
//! from a crew memory dreaming task's run, with the key fixed to that task's
//! key (show also from a run in a memory arm), and a run's activity lists
//! them only then. Propose is tool-only.
use crate::governance::operation::{
    CliArgKind, CliHelp, Effect, NounSpec, OperationSpec, ParamSpec, ParamType,
    ToolOnlyRegistration, UnknownFields, both, cli_only, free_text, mcp_only, operation_registry,
    required,
};
use orbit_types::tool::{McpIdRouteDeclaration, McpToolScope};

operation_registry! {
    noun: MEMORY;
    /// The crew memory verbs.
    pub enum MemoryVerb;
    /// Crew memory operations in CLI order.
    pub const MEMORY_OPERATIONS;
    {
        /// One space's current or chosen revision, or every space's summary.
        Show => SHOW,
        /// One space's revisions and audit events.
        History => HISTORY,
        /// Render every current space to Markdown files for reading.
        Export => EXPORT,
        /// Replace one space's files with a new revision (operator only).
        Write => WRITE,
        /// Empty one space with a new revision (operator only).
        Reset => RESET,
        /// Copy one model's space into another model's space (operator only).
        Copy => COPY,
        /// Propose a note for the global memory (dreaming runs only).
        Propose => PROPOSE,
    }
}

/// The crew memory noun's shared declaration.
pub const MEMORY: NounSpec<MemoryVerb> = NounSpec {
    cli_name: "memory",
    tool_prefix: "orbit.memory",
    tool_only_registration: ToolOnlyRegistration::Inactive,
    cli_about: "Inspect, export and reset crew memory (experimental)",
    audit_target_type: "crew_memory",
    audit_target_param: "key",
    cli_groups: &[],
    operations: MEMORY_OPERATIONS,
};

const KEY: ParamSpec = required(both(
    "key",
    ParamType::String,
    "Memory key: the model id a space belongs to, as runs record it, or `global`.",
    CliArgKind::Positional,
    "Model id, or `global`",
));

const REASON: ParamSpec = free_text(both(
    "reason",
    ParamType::String,
    "Why the space changes; stored on the revision.",
    CliArgKind::flag("reason"),
    "Why the space changes",
));

const SHOW_PARAMS: &[ParamSpec] = &[
    both(
        "key",
        ParamType::String,
        "Memory key: a model id or `global`. Omit to summarise every space.",
        CliArgKind::Positional,
        "Model id, or `global`; omit to summarise every space",
    ),
    both(
        "revision",
        ParamType::Integer,
        "Show this revision instead of the newest. Requires key.",
        CliArgKind::flag("revision"),
        "Show this revision instead of the newest",
    ),
];

const SHOW: OperationSpec<MemoryVerb> = OperationSpec {
    verb: MemoryVerb::Show,
    cli_path: &["show"],
    tool_name: "orbit.memory.show",
    tool_description: "Show a crew memory space: the files of its newest revision (or of `revision`), their digest and size, the evidence the write cited, its writer and reason. Without key, summarise every space's current revision. A key with no revision shows an empty space. Read only; works whether or not crew_memory.enabled is set. An agent may call it only from a crew memory dreaming task's run or a run in a memory experiment arm, and reads only its own model's space (the default key) or `global`.",
    cli_about: "Show a memory space, or summarise every space",
    cli_help: CliHelp::NONE,
    effect: Effect::Read,
    params: &[SHOW_PARAMS],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Refuse,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
    id_route: McpIdRouteDeclaration::Unspecified,
};

const HISTORY: OperationSpec<MemoryVerb> = OperationSpec {
    verb: MemoryVerb::History,
    cli_path: &["history"],
    tool_name: "orbit.memory.history",
    tool_description: "List a crew memory space's revisions, newest first, each with its digest, file names, size, evidence, writer and reason, and the space's audit events in order: revisions written, resets, copies and refused writes with the rule they broke. Read only. Operator CLI only.",
    cli_about: "List a memory space's revisions and audit events",
    cli_help: CliHelp::NONE,
    effect: Effect::Read,
    params: &[&[KEY]],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Refuse,
    mcp_scope: None,
    id_route: McpIdRouteDeclaration::Unspecified,
};

const EXPORT_PARAMS: &[ParamSpec] = &[both(
    "dir",
    ParamType::Path,
    "Directory to write into. Defaults to the workspace's `.orbit/memory`.",
    CliArgKind::flag("dir"),
    "Directory to write into (default: the workspace's .orbit/memory)",
)];

const EXPORT: OperationSpec<MemoryVerb> = OperationSpec {
    verb: MemoryVerb::Export,
    cli_path: &["export"],
    tool_name: "orbit.memory.export",
    tool_description: "Render every crew memory space's current revision as Markdown files for reading: `global.md` and `<model>/<file>.md` under dir. Orbit never reads these files back; changing one changes no memory. Works whether or not crew_memory.enabled is set. Operator CLI only.",
    cli_about: "Write every current space to global.md and <model>/<file>.md",
    cli_help: CliHelp::NONE,
    effect: Effect::Read,
    params: &[EXPORT_PARAMS],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Refuse,
    mcp_scope: None,
    id_route: McpIdRouteDeclaration::Unspecified,
};

const WRITE_PARAMS: &[ParamSpec] = &[
    required(both(
        "key",
        ParamType::String,
        "Memory key: the model id a space belongs to, as runs record it, or `global`. A dreaming run may name only its own task's key.",
        CliArgKind::Positional,
        "Model id, or `global`",
    )),
    mcp_only(
        "files",
        ParamType::Array,
        "The space's complete new content as {path, text} objects. It replaces every file; there is no append.",
    ),
    cli_only(
        "file_paths",
        ParamType::StringList,
        CliArgKind::Flag {
            long: "file",
            delimiter: None,
            aliases: &[],
            visible_aliases: &[],
            value_name: Some("PATH"),
            multiple: false,
        },
        "A Markdown file of the new space, stored under its file name; repeatable. The files given replace the whole space",
    ),
    both(
        "evidence",
        ParamType::StringList,
        "Evidence task ids the notes rest on. A dreaming run must cite at least one, each from its evidence pack; an operator write may cite none.",
        CliArgKind::Flag {
            long: "evidence",
            delimiter: Some(','),
            aliases: &[],
            visible_aliases: &[],
            value_name: Some("TASK_ID"),
            multiple: false,
        },
        "Evidence task id the notes rest on; repeatable or comma-separated",
    ),
    REASON,
];

const WRITE: OperationSpec<MemoryVerb> = OperationSpec {
    verb: MemoryVerb::Write,
    cli_path: &["write"],
    tool_name: "orbit.memory.write",
    tool_description: "Replace a crew memory space's files with a new revision. Refused while crew_memory.enabled is false. A write is refused whole, naming the rule, when it breaks caps (a model space at most 8 files and 6 KiB, global 1 file and 3 KiB, names [a-z0-9-]{1,40}.md), provenance (a dreaming run cites at least one evidence task id, each from its evidence pack), the score-blind lint (scorecard vocabulary or another crew's or model's name), the not-instructions lint (a URL, an Orbit tool name, a shell command block or an injection phrase) or rewrite-whole (each file once); a refusal is recorded in the space's history. An agent may write only from a crew memory dreaming task's run, and only that task's own key: another model's key and `global` are refused (propose a global note instead).",
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

const RESET: OperationSpec<MemoryVerb> = OperationSpec {
    verb: MemoryVerb::Reset,
    cli_path: &["reset"],
    tool_name: "orbit.memory.reset",
    tool_description: "Empty a crew memory space by appending a revision with no files; earlier revisions stay in history. Refused while crew_memory.enabled is false. Operator CLI only.",
    cli_about: "Empty a memory space with a new revision",
    cli_help: CliHelp::NONE,
    effect: Effect::Write,
    params: &[&[KEY, REASON]],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Refuse,
    mcp_scope: None,
    id_route: McpIdRouteDeclaration::Unspecified,
};

const COPY_PARAMS: &[ParamSpec] = &[
    required(both(
        "from",
        ParamType::String,
        "Model id whose current space is copied.",
        CliArgKind::Positional,
        "Model id to copy from",
    )),
    required(both(
        "to",
        ParamType::String,
        "Model id whose space the copy replaces, such as an upgraded model.",
        CliArgKind::Positional,
        "Model id to copy to",
    )),
    REASON,
];

const COPY: OperationSpec<MemoryVerb> = OperationSpec {
    verb: MemoryVerb::Copy,
    cli_path: &["copy"],
    tool_name: "orbit.memory.copy",
    tool_description: "Copy one model's current crew memory space into another model's space as a new revision, for a model upgrade. The copy keeps the source revision's evidence and passes the same write rules as any write. Neither side may be global. Refused while crew_memory.enabled is false. Operator CLI only.",
    cli_about: "Copy one model's space into another model's space",
    cli_help: CliHelp::NONE,
    effect: Effect::Write,
    params: &[COPY_PARAMS],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Refuse,
    mcp_scope: None,
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
    tool_description: "Propose a note for the global memory every model receives, for an operator to accept or reject. Only a crew memory dreaming task's run may propose, at most 2 proposals per run. The note is refused, naming the rule, when it breaks the global memory's caps (3 KiB), provenance (at least one evidence task id, each from the run's evidence pack), the score-blind lint or the not-instructions lint. Refused while crew_memory.enabled is false.",
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
