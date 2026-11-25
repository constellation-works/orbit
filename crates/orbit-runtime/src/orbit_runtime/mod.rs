//! [`RuntimeKernel`]: shared runtime types and method groups for construction,
//! identity, host probes, persistence, configuration, and internal accessors.

mod accessors;
pub(crate) mod admission_policy;
pub use admission_policy::AdmissionCrewSettings;
mod build;
mod config;
mod host;
mod identity;
mod persistence;
mod validation_warning;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub use crate::base::RuntimeConfigLoader;

use crate::application_port::ApplicationPort;
use crate::run::Run;

/// One-shot callback for the next orphaned-leaf reconciliation on a runtime.
type OrphanReconcileCallback = Box<dyn FnOnce() + Send>;
type OrphanReconcileHook = Arc<Mutex<Option<OrphanReconcileCallback>>>;

/// The whole runtime of one process: the four layers, reached through
/// [`Self::run`], [`Self::host`], [`Self::stored`] and [`Self::base`], plus
/// the application callback and the test seams that are not yet layered.
#[derive(Clone)]
pub struct RuntimeKernel {
    /// The application operations this kernel calls back into, supplied by
    /// the composition root that builds it.
    pub(crate) application: &'static dyn ApplicationPort,
    /// The top layer, which holds the three below it.
    run: Run,
    /// One-shot hook run at the start of the next orphaned-leaf reconciliation.
    /// The pull-refill boundary test persists a graceful cancel there. Empty
    /// in production. Instance-scoped so concurrent tests cannot collide.
    orphan_reconcile_hook: OrphanReconcileHook,
    /// Test-only seam for approve/start/reject: after the locked `get_task`,
    /// mutate the named task so compare-and-set can observe a lost race.
    /// Instance-scoped so concurrent `cargo test` threads cannot collide on
    /// the deterministic first task ID.
    #[cfg(any(test, feature = "test-support"))]
    transition_read_hook: Arc<Mutex<Option<(String, orbit_types::task::TaskStatus)>>>,
    /// Test-only seam for the dashboard archive race: when set to a task id,
    /// the archive path inserts a non-terminal local run owning that task
    /// after the outer ownership pre-check, so the under-lock recheck can
    /// observe the lost race. Instance-scoped like the read hook above.
    #[cfg(any(test, feature = "test-support"))]
    transition_run_hook: Arc<Mutex<Option<String>>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrbitRuntimeRoots {
    pub global_root: PathBuf,
    pub shared_root: PathBuf,
    pub local_root: PathBuf,
}

/// Whether the constructing process retains this runtime past a single command.
///
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostLifetime {
    ShortLived,
    LongLived,
}
