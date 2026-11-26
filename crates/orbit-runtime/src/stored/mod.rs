//! [`Stored`]: [`Base`] plus the opened stores — what an operation that reads
//! or writes a record needs.

mod accessors;
pub mod audit;
mod authorization;
pub mod friction;
pub mod job_runs;
pub mod locks;
mod persistence;
pub mod recovery_authority;
mod reservation_history;
pub mod stores;
pub mod task_access;
pub mod workspace;

use std::sync::Arc;

use self::stores::OrbitStores;
use self::task_access::{OwnerRoute, TaskAccess, TaskRoute};
use self::workspace::catalog;
use crate::base::Base;
use crate::base::role::Role;

/// The layer an operation that reads or writes a record runs against.
#[derive(Clone)]
pub struct Stored {
    pub(crate) stores: OrbitStores,
    /// Where task reads go: this checkout's store, or the drain owner for a
    /// claimed worker. Chosen once from the base's role; see
    /// [`Self::set_role`] for the builders that re-resolve it.
    task_route: TaskRoute,
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
    pub(crate) layout_report: Arc<orbit_store::migrate::layout::LayoutUpgradeReport>,
    // Declared last so the stores close before the base's roots are dropped.
    base: Base,
}

impl Stored {
    /// The stores as a runtime opens them: with write access and no
    /// cross-workspace catalog.
    pub(crate) fn new(
        base: Base,
        stores: OrbitStores,
        layout_report: orbit_store::migrate::layout::LayoutUpgradeReport,
    ) -> Self {
        Self {
            stores,
            task_route: TaskRoute::for_role(base.role(), None),
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

    /// This process's task reads, through the route chosen for its role.
    pub fn task(&self) -> TaskAccess<'_> {
        TaskAccess::new(self)
    }

    /// The route to the drain owner, when this process is a claimed worker.
    pub(crate) fn owner_route(&self) -> Option<&OwnerRoute> {
        self.task_route.owner()
    }

    /// Replace the role a kernel builder re-resolved, and choose the task
    /// route again for it. A worker keeps the coordinator it already has.
    pub(crate) fn set_role(&mut self, role: Role) {
        let coordinator = self.owner_route().and_then(OwnerRoute::coordinator);
        self.task_route = TaskRoute::for_role(&role, coordinator);
        self.base.set_role(role);
    }

    /// Attach the coordinator a worker's owner route calls. A process that is
    /// not a claimed worker has no owner route and keeps none.
    pub(crate) fn set_owner_coordinator(
        &mut self,
        coordinator: Arc<dyn orbit_tools::OwnerCoordinator>,
    ) {
        if let Some(route) = self.task_route.owner_mut() {
            route.set_coordinator(coordinator);
        }
    }

    pub(crate) fn set_execution_location(
        &mut self,
        location: Option<orbit_types::task::ExecutionLocation>,
    ) {
        self.stores.job_run = self.stores.job_run.with_execution_location(location);
    }
}
