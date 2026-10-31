//! Per-surface artifact-listing hints for artifact-get errors.
//!
//! `orbit.task.artifact.get` serves both the CLI (`orbit task artifact get`)
//! and MCP (`orbit_task_artifact_get`) through one handler. A hint naming the
//! listing action must use the caller's own surface syntax: the MCP adapter
//! mints an origin session id for every session it serves, while
//! `orbit tool run` and in-process callers carry none, so they stay CLI.

use orbit_types::tool::ToolSessionContext;

/// List-before-read remedy in MCP tool-call syntax.
pub const ARTIFACT_LISTING_HINT_MCP: &str =
    "list available artifacts with orbit.task.show and field: \"artifacts\"";

/// List-before-read remedy in CLI syntax, matching
/// `orbit task artifact get --help`.
pub const ARTIFACT_LISTING_HINT_CLI: &str =
    "list available artifacts with orbit task show <ID> --fields artifacts";

/// The listing hint for the calling surface.
pub fn artifact_listing_hint(session: &ToolSessionContext) -> &'static str {
    if session.origin_session_id.is_some() {
        ARTIFACT_LISTING_HINT_MCP
    } else {
        ARTIFACT_LISTING_HINT_CLI
    }
}
