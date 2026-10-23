//! Nested task operations, preserving the shipped CLI and MCP contracts.

use super::operations::{TaskOperation, TaskVerb};
use crate::governance::operation::{
    CliArgKind, CliConstraint, CliHelp, Effect, ParamType, UnknownFields, aliased, both, cli_only,
    defaulted, mcp_only, required,
};
use orbit_types::tool::McpToolScope;

pub(super) const ARTIFACT_PUT: TaskOperation = TaskOperation {
    verb: TaskVerb::ArtifactPut,
    cli_path: &["artifact", "put"],
    tool_name: "orbit.task.artifact.put",
    tool_description: "Store a file (any content, up to 1 MiB) as a task artifact. Sources must stay inside the workspace after symlink resolution; write evidence under `.orbit/tmp/`. See `docs/design/task-artifacts/2_design.md#evidence-contracts` and the assigned task's evidence template for automation coverage and review report contracts.",
    cli_about: "Store a source file under a task's artifacts directory",
    cli_help: CliHelp {
        long_about: None,
        after_help: None,
        flag_order: &[],
        constraints: &[],
    },
    effect: Effect::Write,
    params: &[&[
        required(both(
            "id",
            ParamType::String,
            "task ID",
            CliArgKind::Positional,
            "Task ID",
        )),
        required(aliased(
            both(
                "source_path",
                ParamType::String,
                "Source file to store as a task artifact. Resolved against the \
                    caller cwd, not the checkout root, so a file that exists only at the \
                    checkout root is refused with the absolute path to pass. Then confined \
                    to the workspace checkout after symlink resolution. Absolute paths and symlinks that escape the workspace are \
                    rejected. Write evidence under `.orbit/tmp/` (`$ORBIT_SCRATCH_DIR`); \
                    sources under `/tmp` are outside the workspace and are refused.",
                CliArgKind::Positional,
                "Source file to store as a task artifact, up to 1 MiB. Any content is accepted; the media type comes from the file name, and unknown extensions are stored as application/octet-stream. Resolved against the current directory, not the checkout root, so a file that exists only at the checkout root is refused with the absolute path to pass. Then confined to the workspace checkout after symlink resolution: sources that escape it, including under `/tmp`, are refused, so write evidence under `.orbit/tmp/` (`$ORBIT_SCRATCH_DIR`)",
            ),
            &["sourcePath", "source-path"],
        )),
        aliased(
            both(
                "path",
                ParamType::String,
                "Artifact path relative to the task artifacts directory. Defaults to the source file name.",
                CliArgKind::Flag {
                    long: "path",
                    delimiter: None,
                    aliases: &[],
                    visible_aliases: &[],
                    value_name: Some("ARTIFACT_PATH"),
                    multiple: false,
                },
                "Artifact path relative to the task artifacts directory. Defaults to the source file name",
            ),
            &["artifact_path", "artifactPath"],
        ),
        both(
            "model",
            ParamType::String,
            "Preferred provenance field. Pass the canonical agent family (codex, claude, gemini, or grok); full model strings are accepted and auto-normalized.",
            CliArgKind::flag("model"),
            "Explicit agent model to persist on the task artifact update",
        ),
    ]],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
};

pub(super) const ARTIFACT_GET: TaskOperation = TaskOperation {
    verb: TaskVerb::ArtifactGet,
    cli_path: &["artifact", "get"],
    tool_name: "orbit.task.artifact.get",
    tool_description: "Read one stored task artifact by owning task ID and artifact path. Returns \
                 `media_type`, `size`, and the bytes: UTF-8 text as `content`, and everything \
                 else as base64 in `content_base64`. Supported raster images (PNG, JPEG, GIF, \
                 WebP) whose bytes match their declared type are additionally returned as a \
                 viewable image on transports that carry image content, so a multimodal client \
                 can inspect a stored visual reference directly. SVG, HTML, and any payload \
                 whose bytes contradict its media type are returned as opaque base64 and are \
                 never rendered. List available artifacts first with `orbit.task.show` and \
                 `field: \"artifacts\"`; payloads are fetched only through this call. Reads are \
                 bounded to 1048576 bytes.",
    cli_about: "Read one stored artifact back out of a task",
    cli_help: CliHelp {
        long_about: None,
        after_help: None,
        flag_order: &[],
        constraints: &[],
    },
    effect: Effect::Read,
    params: &[
        &[
            required(both(
                "id",
                ParamType::String,
                "Globally unique task ID that owns the artifact.",
                CliArgKind::Positional,
                "Task ID that owns the artifact",
            )),
            required(aliased(
                both(
                    "path",
                    ParamType::String,
                    "Artifact path relative to the task artifacts directory, exactly as \
                    it appears in `orbit.task.show` with `field: \"artifacts\"`. Absolute paths \
                    and `..` are rejected.",
                    CliArgKind::Positional,
                    "Artifact path relative to the task artifacts directory, as listed by `orbit task show <ID> --fields artifacts`",
                ),
                &["artifact_path", "artifactPath"],
            )),
            cli_only(
                "out",
                ParamType::Path,
                CliArgKind::flag("out"),
                "Write the artifact's bytes to this file instead of printing them",
            ),
        ],
        super::operations::IDENTITY_PARAMS,
        &[mcp_only(
            "workspace",
            ParamType::String,
            "Optional explicit workspace filter. `id` is resolved globally by \
                default; when supplied, a registered workspace name, logical workspace ID \
                (`ws_*`), or absolute local checkout path is fail-closed — a valid workspace that \
                does not own the task returns not-found.",
        )],
    ],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
};

pub(super) const LOCKS_LIST: TaskOperation = TaskOperation {
    verb: TaskVerb::LocksList,
    cli_path: &["locks", "list"],
    tool_name: "orbit.task.locks",
    tool_description: "List files currently locked by active Orbit tasks and reservations as JSON.",
    cli_about: "Show files locked by active (in-progress/review) tasks and reservations",
    cli_help: CliHelp {
        long_about: None,
        after_help: None,
        flag_order: &[],
        constraints: &[],
    },
    // Listing expires reservations and records their expiry.
    effect: Effect::Write,
    params: &[&[]],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: None,
};

pub(super) const LOCKS_RESERVE: TaskOperation = TaskOperation {
    verb: TaskVerb::LocksReserve,
    cli_path: &["locks", "reserve"],
    tool_name: "orbit.task.locks.reserve",
    tool_description: "Attempt to atomically reserve either task-derived context files (`task_ids`) or \
                 direct file/directory selectors (`files`) and return either a reservation grant \
                 or a conflict list. Exactly one of `task_ids` or `files` must be provided.",
    cli_about: "Claim a file surface for a bounded window before starting work on it",
    cli_help: CliHelp {
        long_about: None,
        after_help: Some(
            "Examples:\n  orbit task locks reserve --task <TASK_ID>\n  orbit task locks reserve --file dir:crates/orbit-cli --ttl 2h\n  orbit task locks reserve --file file:README.md --json\n\n\
                  Reserving is how work outside the task pipeline takes the same locks the\n\
                  pipeline takes: an editor session, a manual migration, a long refactor. The\n\
                  claim is atomic and either grants the whole surface or grants nothing and\n\
                  reports who holds the overlap.\n\n\
                  `--task` reserves that task's declared context surface, pruned and expanded\n\
                  exactly as conflict admission expands it. `--file` reserves selectors\n\
                  directly. Exactly one of the two — they are mutually exclusive forms.\n\n\
                  Reservations expire on their own, so the TTL is the safety net for a session\n\
                  that dies holding one. Release early with `orbit task locks release <id>\n\
                  --confirm`; a denied reservation exits 3.\n\n\
                  Reserving requires the `operator` or `runner` capability, same as releasing:\n\
                  a caller that can lock a surface out from under everyone else must be the same\n\
                  caller trusted to clear it. From a plain shell, set `ORBIT_OPERATOR=1`.",
        ),
        flag_order: &[],
        constraints: &[
            CliConstraint::ConflictsWith("task_ids", "files"),
            CliConstraint::RequiredUnlessPresent("task_ids", "files"),
        ],
    },
    effect: Effect::Write,
    params: &[
        &[
            aliased(
                both(
                    "task_ids",
                    ParamType::StringList,
                    "Task IDs in the bundle as a string or array of strings.",
                    CliArgKind::Flag {
                        long: "task",
                        delimiter: Some(','),
                        aliases: &[],
                        visible_aliases: &[],
                        value_name: Some("TASK_ID"),
                        multiple: false,
                    },
                    "Task whose declared context surface to reserve. Repeat or comma-separate to claim a bundle's combined surface",
                ),
                &["taskIds", "task-ids"],
            ),
            both(
                "files",
                ParamType::StringList,
                "Canonical file or directory selectors to reserve (`file:...`, `dir:...`).",
                CliArgKind::Flag {
                    long: "file",
                    delimiter: Some(','),
                    aliases: &[],
                    visible_aliases: &[],
                    value_name: Some("SELECTOR"),
                    multiple: false,
                },
                "Selector to reserve directly (`file:...`, `dir:...`). Repeat or comma-separate for several",
            ),
            aliased(
                mcp_only(
                    "ttl_seconds",
                    ParamType::UnsignedInteger,
                    "Optional reservation TTL in seconds. Defaults to 1800; max 7200.",
                ),
                &["ttlSeconds", "ttl-seconds"],
            ),
            defaulted(
                cli_only(
                    "ttl",
                    ParamType::String,
                    CliArgKind::Flag {
                        long: "ttl",
                        delimiter: None,
                        aliases: &[],
                        visible_aliases: &[],
                        value_name: Some("DURATION"),
                        multiple: false,
                    },
                    "How long the claim holds, e.g. `45m`, `2h`. Default 30m, maximum 2h",
                ),
                "30m",
            ),
        ],
        super::operations::IDENTITY_PARAMS,
    ],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: None,
};

pub(super) const LOCKS_RELEASE: TaskOperation = TaskOperation {
    verb: TaskVerb::LocksRelease,
    cli_path: &["locks", "release"],
    tool_name: "orbit.task.locks.release",
    tool_description: "Release a previously granted task lock reservation.",
    cli_about: "Release a stale task lock reservation (operator/admin escape hatch)",
    cli_help: CliHelp {
        long_about: None,
        after_help: None,
        flag_order: &[],
        constraints: &[],
    },
    effect: Effect::Write,
    params: &[
        &[
            required(aliased(
                both(
                    "reservation_id",
                    ParamType::String,
                    "Reservation ID to release.",
                    CliArgKind::Positional,
                    "Reservation ID to release (see `orbit task locks list --json`)",
                ),
                &["reservationId", "reservation-id"],
            )),
            cli_only(
                "confirm",
                ParamType::Switch,
                CliArgKind::flag("confirm"),
                "Confirm release of the reservation",
            ),
        ],
        super::operations::IDENTITY_PARAMS,
    ],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: None,
};
