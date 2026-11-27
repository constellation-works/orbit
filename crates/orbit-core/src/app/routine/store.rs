//! Routines [ORB-10021]: Orbit as the constellation's single scheduler.
//!
//! A routine is a durable YAML definition of recurring work — a cron trigger,
//! a catalog target, and a retry/overlap policy — living under
//! `.orbit/routines/` in a registered owner checkout. That directory is
//! per-user state, not a repository artifact. The stateless
//! [`run_sweep`](super::run_sweep) pass, invoked every minute by the OS clock
//! (see [`orbit_clock`]), fires whatever is due on this host through the existing
//! v2 run machinery. Every owner checkout is an independent schedule; a
//! replica checkout schedules only host-local worktree GC [ORB-14173] and
//! store GC [ORB-16354] (design in `docs/design/routines/`).

use std::path::Path;

use orbit_common::OrbitError;
use orbit_store::Store;

use crate::application::store_opener::StoreOpener;

/// Who this machine is, as reported by routine status and sweep output.
/// Routine eligibility no longer consults it: it identifies the machine whose
/// store and clock a pass acted on, for display and audit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutineMachineIdentity {
    /// Stable machine identity (`machine.id`).
    pub machine_id: String,
    /// Operator-facing machine name (`machine.name`).
    pub machine_name: String,
}

/// Open the config-resolved machine-local scheduler store for a mutating route.
/// Creates and migrates it on first use.
pub(super) fn open_routine_store(
    opener: StoreOpener,
    global_root: &Path,
) -> Result<Store, OrbitError> {
    let database = orbit_config::resolved_audit_db_path(
        &orbit_common::Env::capture(),
        &orbit_config::ConfigRoots::global_only(global_root),
    )?;
    opener.open(&database)
}

/// This host's routine circuit breakers in its scheduler store, keyed by
/// routine name.
pub(crate) fn host_routine_breakers(
    store: &Store,
) -> Result<
    std::collections::BTreeMap<String, orbit_store::contract::RoutineBreakerRecord>,
    OrbitError,
> {
    store.read()?.routine().breakers()
}

/// Create and migrate the machine's scheduler store when it is absent. `orbit
/// init` calls this as the first-run setup step, so reads that follow it find
/// an existing store (STD-01 R31).
pub fn ensure_routine_store(opener: StoreOpener, global_root: &Path) -> Result<(), OrbitError> {
    open_routine_store(opener, global_root).map(|_| ())
}

/// Open the existing scheduler store for a read-only projection. A read never
/// creates or migrates it (STD-01 R31): a missing store is setup that has not
/// been done, and the error names `orbit init`, which owns first-run creation.
pub(super) fn open_existing_routine_store(
    opener: StoreOpener,
    global_root: &Path,
) -> Result<Store, OrbitError> {
    let database = orbit_config::resolved_audit_db_path(
        &orbit_common::Env::capture(),
        &orbit_config::ConfigRoots::global_only(global_root),
    )?;
    if !database.exists() {
        return Err(OrbitError::InvalidInput(format!(
            "routine scheduler store {} does not exist; run `orbit init` on this machine to create it",
            database.display()
        )));
    }
    opener.open_read_only(&database)
}
