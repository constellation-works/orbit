//! Keep a build that is not the host's installed Orbit from migrating shared
//! state past what the installed executable can keep using.
//!
//! A development, candidate or QA build run under the operator's home opens
//! the same global store, and from a run worktree the same primary workspace
//! `.orbit`, as the installed binary. Generation admission only protects
//! processes that are live, and short-lived CLI commands rarely are, so such
//! a build could apply a breaking migration and lock every later installed
//! command out of the host [ORB-20262]. Before it can, this guard asks the
//! installed executable (in the managed install directory) which state it
//! supports, and refuses a migration that it could not keep writing through.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::fs::generation::{
    CompatibilityIdentity, LedgerCompatibility, installed_executable_other_than_this_process,
    reported_compatibility,
};
use orbit_store::Store;
use orbit_store::compose::compiled_compatibility;
use orbit_store::workflow::layout::current_layout_version;

/// One state ledger this process would migrate past the installed binary.
struct Advance {
    ledger: &'static str,
    location: String,
    current: u32,
    /// The newest migration this process would apply that older writers
    /// cannot keep writing through.
    breaking: u32,
    select: fn(&CompatibilityIdentity) -> LedgerCompatibility,
}

/// Refuse, before writing, a migration of the workspace layout under
/// `shared_root` or the store schema under `global_root` that the installed
/// Orbit could no longer write through, when this process is not the
/// installed executable. Without an installed executable, or when it does not
/// report its compatibility, nothing is refused.
pub(crate) fn verify_installed_binary_keeps_state(
    global_root: &Path,
    shared_root: &Path,
) -> Result<(), OrbitError> {
    let Some(installed) = installed_executable_other_than_this_process() else {
        return Ok(());
    };
    let compiled = compiled_compatibility();
    let mut advances = Vec::new();
    let layout = current_layout_version(shared_root)?;
    if let Some(breaking) = breaking_advance(layout, compiled.workspace_layout) {
        advances.push(Advance {
            ledger: "workspace layout",
            location: shared_root.display().to_string(),
            current: layout,
            breaking,
            select: |identity| identity.workspace_layout,
        });
    }
    let database = global_root.join("orbit.db");
    if database.is_file()
        && let Ok(schema) =
            Store::open_read_only(&database).and_then(|store| store.schema_version())
        && let Some(breaking) = breaking_advance(schema, compiled.store_schema)
    {
        advances.push(Advance {
            ledger: "store schema",
            location: database.display().to_string(),
            current: schema,
            breaking,
            select: |identity| identity.store_schema,
        });
    }
    if advances.is_empty() {
        return Ok(());
    }
    let Some(installed_identity) = reported_compatibility(&installed) else {
        tracing::warn!(
            target: "orbit.core.bootstrap",
            installed = %installed.display(),
            "the installed orbit did not report its state compatibility; applying migrations \
             without checking that it can keep using the state"
        );
        return Ok(());
    };
    let running = std::env::current_exe().map_or_else(
        |_| "this orbit".to_string(),
        |path| format!("'{}'", path.display()),
    );
    for advance in advances {
        let supported = (advance.select)(&installed_identity).version;
        if advance.breaking > supported {
            return Err(OrbitError::Execution(format!(
                "upgrade admission refused: {running} is not the installed orbit '{installed}' \
                 and would migrate the {ledger} at '{location}' from v{current} past v{breaking}, \
                 which the installed orbit cannot keep using: it supports {ledger} v{supported}. \
                 Nothing was changed. Run development and QA builds under a disposable home with \
                 their own workspace, or install this build first with \
                 `orbit update --local-candidate` so the installed orbit applies the migration",
                installed = installed.display(),
                ledger = advance.ledger,
                location = advance.location,
                current = advance.current,
                breaking = advance.breaking,
            )));
        }
    }
    Ok(())
}

/// The newest migration above `current` that older writers cannot keep
/// writing through, when opening at `current` would apply one. A fresh ledger
/// (version 0) has no users yet.
fn breaking_advance(current: u32, compiled: LedgerCompatibility) -> Option<u32> {
    let breaking = compiled.writer_floor.max(compiled.reader_floor);
    (current > 0 && breaking > current).then_some(breaking)
}
