//! [`Stored`]: [`Base`] plus the opened stores — what an operation that reads
//! or writes a record needs.

use std::sync::Arc;

use crate::base::Base;
use crate::context::OrbitStores;
use crate::workspace::catalog;

/// The layer an operation that reads or writes a record runs against.
#[derive(Clone)]
pub struct Stored {
    pub(crate) stores: OrbitStores,
    /// This process joined a live executable generation other than its own
    /// without recording itself, so it opened Orbit's state without write
    /// access. Reads work; operational state mutations must be skipped. Tool
    /// dispatch may append an audit row through a separate migration-free path.
    pub(crate) write_free: bool,
    /// Supplied by the registry-owning composition layer, for reads that
    /// span more than one workspace. Absent on a standalone runtime, which
    /// then answers only for its own checkout [ORB-11027].
    pub(crate) workspace_catalog: Option<Arc<dyn catalog::WorkspaceCatalog>>,
    /// Outcome of the [ORB-10012] workspace-layout pre-flight that ran when
    /// this runtime opened (empty `applied` when the layout was already
    /// current). Surfaced by `orbit migrate`.
    pub(crate) layout_report: Arc<orbit_store::workflow::layout::LayoutUpgradeReport>,
    // Declared last so the stores close before the base's roots are dropped.
    base: Base,
}

impl Stored {
    /// The stores as a runtime opens them: with write access and no
    /// cross-workspace catalog.
    pub(crate) fn new(
        base: Base,
        stores: OrbitStores,
        layout_report: orbit_store::workflow::layout::LayoutUpgradeReport,
    ) -> Self {
        Self {
            stores,
            write_free: false,
            workspace_catalog: None,
            layout_report: Arc::new(layout_report),
            base,
        }
    }

    /// The layer below.
    pub fn base(&self) -> &Base {
        &self.base
    }

    pub(crate) fn base_mut(&mut self) -> &mut Base {
        &mut self.base
    }

    /// The opened stores.
    pub fn stores(&self) -> &OrbitStores {
        &self.stores
    }

    pub(crate) fn set_execution_location(
        &mut self,
        location: Option<orbit_types::task::ExecutionLocation>,
    ) {
        self.stores.job_run = self.stores.job_run.with_execution_location(location);
    }
}
