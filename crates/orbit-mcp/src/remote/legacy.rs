//! Retired destination-side caller authorization, kept only as a warning
//! [ORB-12564].
//!
//! Orbit is a single-user tool, and an SSH login to a destination is ownership
//! of it: anyone who can run `ssh box "orbit mcp serve --operator"` can equally
//! run `ssh box "ORBIT_OPERATOR=1 orbit tool run …"` or rewrite the ceiling
//! file to grant themselves a row. The destination therefore honors the
//! authority in the argv it was started with, exactly as it does for a local
//! session, and the two files that used to cap a remote session no longer
//! participate in any decision.
//!
//! Their *presence* is still worth one line of output. A destination upgraded
//! from the previous model keeps a file whose whole purpose was to refuse
//! callers; silently ignoring it would leave an operator believing a ceiling is
//! in force that nothing reads. Startup warns once and continues — a leftover
//! file is not a reason to refuse a session — and `orbit doctor` says what to
//! delete.

use std::path::{Path, PathBuf};

/// The retired Tier 1 callers file, relative to the machine-global Orbit dir.
pub(super) const LEGACY_CALLERS_FILE: &str = "mcp-callers.toml";

/// The retired Tier 2 acceptance directory, relative to the same root.
pub(super) const LEGACY_ACCEPTANCE_DIR: &str = "mcp-ssh-acceptance";

/// Retired caller-authorization paths that still exist under `global_root`.
///
/// Existence is the whole test: a malformed or unreadable leftover is as
/// ignored as a well-formed one, so nothing here parses, opens, or validates.
pub fn ignored_caller_authorization_paths(global_root: &Path) -> Vec<PathBuf> {
    [LEGACY_CALLERS_FILE, LEGACY_ACCEPTANCE_DIR]
        .into_iter()
        .map(|name| global_root.join(name))
        .filter(|path| path.exists())
        .collect()
}

/// Warn once, naming every retired file this machine still carries.
///
/// One record per server start, not one per file: an operator needs to know
/// that the ceiling they wrote is inert, and repeating that per path would
/// only make the line they have to read longer.
pub fn warn_ignored_caller_authorization(global_root: &Path) {
    let ignored = ignored_caller_authorization_paths(global_root);
    if ignored.is_empty() {
        return;
    }
    let paths = ignored
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    tracing::warn!(
        target: "orbit.mcp.remote",
        paths = %paths,
        "destination-side caller authorization was removed; these files are ignored and grant or \
         refuse nothing. An MCP session served over SSH now holds the authority its argv asks \
         for, the same as a local one. Delete them; `orbit doctor` repeats this."
    );
}
