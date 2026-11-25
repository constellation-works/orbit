//! Store facade: consistent read snapshots (`Store::read`) and IMMEDIATE write
//! transactions (`Store::write`) over the host SQLite driver.

use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::{Connection, TransactionBehavior};

use crate::Store;
use crate::driver::sqlite::host::read_pool::ReadGuard;
use crate::repository::admission_load::AdmissionLoadRepository;
use crate::repository::application_consent::ApplicationConsentRepository;
use crate::repository::backlog_forecast::BacklogForecastRepository;
use crate::repository::codebase_fact::{AgentFactRepository, CodebaseFactRepository};
use crate::repository::crew_memory::CrewMemoryRepository;
use crate::repository::evolution::EvolutionRepository;
use crate::repository::experiment::{ExperimentAssignmentRepository, ExperimentRepository};
use crate::repository::landed_diff::LandedDiffRepository;
use crate::repository::notice::NoticeRepository;
use crate::repository::provider_limit::ProviderLimitRepository;
use crate::repository::reliability::ReliabilityRepository;
use crate::repository::resource_usage::{ResourceUsageRepository, WorkerResourceLimitRepository};
use crate::repository::retention::RetentionRepository;

/// One consistent read view of the host store.
///
/// Opens a `BEGIN DEFERRED` transaction on a pooled reader connection (or the
/// in-memory writer connection) so multiple repository queries observe the same
/// snapshot, and rolls it back on drop before returning the connection to the
/// pool.
pub struct Snapshot<'a> {
    conn: ReadGuard<'a>,
}

impl Snapshot<'_> {
    pub(crate) fn connection(&self) -> &Connection {
        &self.conn
    }

    /// Read-only access to host admission-load records in this snapshot.
    pub fn admission_load(&self) -> AdmissionLoadRepository<'_, Self> {
        AdmissionLoadRepository::for_snapshot(self)
    }

    /// Read-only access to stored agent task-fact cache rows in this snapshot.
    pub fn agent_fact(&self) -> AgentFactRepository<'_, Self> {
        AgentFactRepository::for_snapshot(self)
    }

    /// Read-only access to application consent records in this snapshot.
    pub fn application_consent(&self) -> ApplicationConsentRepository<'_, Self> {
        ApplicationConsentRepository::for_snapshot(self)
    }

    /// Read-only access to stored backlog forecast records in this snapshot.
    pub fn backlog_forecast(&self) -> BacklogForecastRepository<'_, Self> {
        BacklogForecastRepository::for_snapshot(self)
    }

    /// Read-only access to stored codebase task-fact cache rows in this snapshot.
    pub fn codebase_fact(&self) -> CodebaseFactRepository<'_, Self> {
        CodebaseFactRepository::for_snapshot(self)
    }

    /// Read-only access to evolution ledger records in this snapshot.
    pub fn evolution(&self) -> EvolutionRepository<'_, Self> {
        EvolutionRepository::for_snapshot(self)
    }

    /// Read-only access to landed-delivery diff stat records in this snapshot.
    pub fn landed_diff(&self) -> LandedDiffRepository<'_, Self> {
        LandedDiffRepository::for_snapshot(self)
    }

    /// Read-only access to fleet notice records in this snapshot.
    pub fn notice(&self) -> NoticeRepository<'_, Self> {
        NoticeRepository::for_snapshot(self)
    }
    /// Read-only access to crew memory revisions, events and proposals in this snapshot.
    pub fn crew_memory(&self) -> CrewMemoryRepository<'_, Self> {
        CrewMemoryRepository::for_snapshot(self)
    }

    /// Read-only access to experiment records in this snapshot.
    pub fn experiment(&self) -> ExperimentRepository<'_, Self> {
        ExperimentRepository::for_snapshot(self)
    }

    /// Read-only access to experiment assignments and admissions in this snapshot.
    pub fn experiment_assignment(&self) -> ExperimentAssignmentRepository<'_, Self> {
        ExperimentAssignmentRepository::for_snapshot(self)
    }

    /// Read-only access to this host's provider limits and crew outages in this snapshot.
    pub fn provider_limit(&self) -> ProviderLimitRepository<'_, Self> {
        ProviderLimitRepository::for_snapshot(self)
    }

    /// Read-only access to count-only reliability facts in this snapshot.
    pub fn reliability(&self) -> ReliabilityRepository<'_, Self> {
        ReliabilityRepository::for_snapshot(self)
    }

    /// Read-only access to run and host resource history and admission forecasts in this snapshot.
    pub fn resource_usage(&self) -> ResourceUsageRepository<'_, Self> {
        ResourceUsageRepository::for_snapshot(self)
    }

    /// Read-only access to bounded retention reads and batches in this snapshot.
    pub fn retention(&self) -> RetentionRepository<'_, Self> {
        RetentionRepository::for_snapshot(self)
    }

    /// Read-only access to worker resource-limit breach evidence in this snapshot.
    pub fn worker_resource_limit(&self) -> WorkerResourceLimitRepository<'_, Self> {
        WorkerResourceLimitRepository::for_snapshot(self)
    }
}

impl Drop for Snapshot<'_> {
    fn drop(&mut self) {
        let _ = self.conn.execute_batch("ROLLBACK");
    }
}

/// One `IMMEDIATE` write transaction against the host store.
///
/// Created exclusively by [`Store::write`], which commits when the callback
/// returns `Ok` and rolls back on error or unwind. Writing repository methods
/// are available only on repositories obtained from `&Tx`.
pub struct Tx<'a> {
    conn: &'a Connection,
}

impl Tx<'_> {
    pub(crate) fn connection(&self) -> &Connection {
        self.conn
    }

    /// Read-write access to host admission-load records inside this transaction.
    pub fn admission_load(&self) -> AdmissionLoadRepository<'_, Self> {
        AdmissionLoadRepository::for_tx(self)
    }

    /// Read-write access to stored agent task-fact cache rows inside this
    /// transaction.
    pub fn agent_fact(&self) -> AgentFactRepository<'_, Self> {
        AgentFactRepository::for_tx(self)
    }

    /// Read-write access to application consent records inside this transaction.
    pub fn application_consent(&self) -> ApplicationConsentRepository<'_, Self> {
        ApplicationConsentRepository::for_tx(self)
    }

    /// Read-write access to stored backlog forecast records inside this
    /// transaction.
    pub fn backlog_forecast(&self) -> BacklogForecastRepository<'_, Self> {
        BacklogForecastRepository::for_tx(self)
    }

    /// Read-write access to stored codebase task-fact cache rows inside this
    /// transaction.
    pub fn codebase_fact(&self) -> CodebaseFactRepository<'_, Self> {
        CodebaseFactRepository::for_tx(self)
    }

    /// Read-write access to evolution ledger records inside this transaction.
    pub fn evolution(&self) -> EvolutionRepository<'_, Self> {
        EvolutionRepository::for_tx(self)
    }

    /// Read-write access to landed-delivery diff stat records inside this
    /// transaction.
    pub fn landed_diff(&self) -> LandedDiffRepository<'_, Self> {
        LandedDiffRepository::for_tx(self)
    }

    /// Read-write access to fleet notice records inside this transaction.
    pub fn notice(&self) -> NoticeRepository<'_, Self> {
        NoticeRepository::for_tx(self)
    }
    /// Read-write access to crew memory revisions, events and proposals inside this transaction.
    pub fn crew_memory(&self) -> CrewMemoryRepository<'_, Self> {
        CrewMemoryRepository::for_tx(self)
    }

    /// Read-write access to experiment records inside this transaction.
    pub fn experiment(&self) -> ExperimentRepository<'_, Self> {
        ExperimentRepository::for_tx(self)
    }

    /// Read-write access to experiment assignments and admissions inside this transaction.
    pub fn experiment_assignment(&self) -> ExperimentAssignmentRepository<'_, Self> {
        ExperimentAssignmentRepository::for_tx(self)
    }

    /// Read-write access to this host's provider limits and crew outages inside this transaction.
    pub fn provider_limit(&self) -> ProviderLimitRepository<'_, Self> {
        ProviderLimitRepository::for_tx(self)
    }

    /// Access to count-only reliability facts inside this transaction.
    pub fn reliability(&self) -> ReliabilityRepository<'_, Self> {
        ReliabilityRepository::for_tx(self)
    }

    /// Read-write access to run and host resource history and admission forecasts inside this transaction.
    pub fn resource_usage(&self) -> ResourceUsageRepository<'_, Self> {
        ResourceUsageRepository::for_tx(self)
    }

    /// Read-write access to bounded retention reads and batches inside this transaction.
    pub fn retention(&self) -> RetentionRepository<'_, Self> {
        RetentionRepository::for_tx(self)
    }

    /// Read-write access to worker resource-limit breach evidence inside this transaction.
    pub fn worker_resource_limit(&self) -> WorkerResourceLimitRepository<'_, Self> {
        WorkerResourceLimitRepository::for_tx(self)
    }
}

impl Store {
    /// Open one consistent read view (`BEGIN DEFERRED`) over the host store.
    ///
    /// File-backed stores use the read-only connection pool (`query_only=ON`);
    /// in-memory stores borrow the writer connection, so callers must not open
    /// a [`Snapshot`] while already inside [`Store::write`] or
    /// [`Store::with_transaction`].
    pub fn read(&self) -> Result<Snapshot<'_>, OrbitError> {
        let conn = self.read_connection()?;
        conn.execute_batch("BEGIN DEFERRED")
            .map_err(sqlite_store_error)?;
        Ok(Snapshot { conn })
    }

    /// Execute `op` inside one `IMMEDIATE` write transaction.
    ///
    /// Commits when `op` returns `Ok`, rolls back on `Err` or unwind, and
    /// refuses writes on forward-compatible read-only handles. This is the
    /// only place a [`Tx`] is created.
    pub fn write<T, F>(&self, op: F) -> Result<T, OrbitError>
    where
        F: FnOnce(&Tx<'_>) -> Result<T, OrbitError>,
    {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |store_tx| {
            let tx = Tx {
                conn: store_tx.connection(),
            };
            op(&tx)
        })
    }
}
