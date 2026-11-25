//! Store facade: consistent read snapshots (`Store::read`) and IMMEDIATE write
//! transactions (`Store::write`) over the host SQLite driver.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::{Connection, TransactionBehavior};

use crate::Store;
use crate::contracts::{AuditEventInsertParams, AuditInvocationFields};
use crate::driver::sqlite::host::read_pool::ReadGuard;
use crate::repository::admission_load::AdmissionLoadRepository;
use crate::repository::application_consent::ApplicationConsentRepository;
use crate::repository::audit::{AuditEventRepository, V2AuditRepository};
use crate::repository::automation::AutomationRepository;
use crate::repository::backlog_forecast::BacklogForecastRepository;
use crate::repository::codebase_fact::{AgentFactRepository, CodebaseFactRepository};
use crate::repository::condition::ConditionRepository;
use crate::repository::crew_memory::CrewMemoryRepository;
use crate::repository::drain_intent::DrainIntentRepository;
use crate::repository::evolution::EvolutionRepository;
use crate::repository::experiment::{ExperimentAssignmentRepository, ExperimentRepository};
use crate::repository::friction::FrictionRepository;
use crate::repository::incident::IncidentRepository;
use crate::repository::invocation::InvocationRepository;
use crate::repository::job_run::JobRunRepository;
use crate::repository::landed_diff::LandedDiffRepository;
use crate::repository::lease_notice::LeaseNoticeRepository;
use crate::repository::local_pull::LocalPullRepository;
use crate::repository::notice::NoticeRepository;
use crate::repository::plugin::PluginRepository;
use crate::repository::provider_limit::ProviderLimitRepository;
use crate::repository::reliability::ReliabilityRepository;
use crate::repository::resource_usage::{ResourceUsageRepository, WorkerResourceLimitRepository};
use crate::repository::retention::RetentionRepository;
use crate::repository::review::ReviewRepository;
use crate::repository::routine::RoutineRepository;
use crate::repository::script_grant::ScriptGrantRepository;
use crate::repository::tool::ToolRepository;

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

    /// Read-only access to command audit-event records in this snapshot.
    pub fn audit_event(&self) -> AuditEventRepository<'_, Self> {
        AuditEventRepository::for_snapshot(self)
    }

    /// Read-only access to v2 workflow audit-envelope records in this snapshot.
    pub fn v2_audit(&self) -> V2AuditRepository<'_, Self> {
        V2AuditRepository::for_snapshot(self)
    }

    /// Read-only access to invocation trace and agent-execution records in this snapshot.
    pub fn invocation(&self) -> InvocationRepository<'_, Self> {
        InvocationRepository::for_snapshot(self)
    }

    /// Read-only access to host automation records.
    pub fn automation(&self) -> AutomationRepository<'_, Self> {
        AutomationRepository::for_snapshot(self)
    }

    /// Read-only access to host condition records.
    pub fn condition(&self) -> ConditionRepository<'_, Self> {
        ConditionRepository::for_snapshot(self)
    }

    /// Read-only access to the audit evidence used in incident reports.
    pub fn incident(&self) -> IncidentRepository<'_> {
        IncidentRepository::for_snapshot(self)
    }

    /// Read-only access to host review records.
    pub fn review(&self) -> ReviewRepository<'_, Self> {
        ReviewRepository::for_snapshot(self)
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

    /// Read-only access to workspace-partitioned friction records.
    pub fn friction(&self) -> FrictionRepository<'_, Self> {
        FrictionRepository::for_snapshot(self)
    }

    /// Read-only access to host-local routine records.
    pub fn routine(&self) -> RoutineRepository<'_, Self> {
        RoutineRepository::for_snapshot(self)
    }

    /// Read-only access to fleet notice records in this snapshot.
    pub fn notice(&self) -> NoticeRepository<'_, Self> {
        NoticeRepository::for_snapshot(self)
    }
    /// Read-only access to job runs, their steps, pipeline states, task bindings
    /// and automation action keys in this snapshot.
    pub fn job_run(&self) -> JobRunRepository<'_, Self> {
        JobRunRepository::for_snapshot(self)
    }
    /// Read-only access to local pull admissions in this snapshot.
    pub fn local_pull(&self) -> LocalPullRepository<'_, Self> {
        LocalPullRepository::for_snapshot(self)
    }
    /// Read-only access to workspace drain intents in this snapshot.
    pub fn drain_intent(&self) -> DrainIntentRepository<'_, Self> {
        DrainIntentRepository::for_snapshot(self)
    }
    /// Read-only access to lease notices in this snapshot.
    pub fn lease_notice(&self) -> LeaseNoticeRepository<'_, Self> {
        LeaseNoticeRepository::for_snapshot(self)
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

    /// Read-only access to host-local installed-plugin records in this snapshot.
    pub fn plugin(&self) -> PluginRepository<'_, Self> {
        PluginRepository::for_snapshot(self)
    }

    /// Read-only access to host-local script grant records in this snapshot.
    pub fn script_grant(&self) -> ScriptGrantRepository<'_, Self> {
        ScriptGrantRepository::for_snapshot(self)
    }

    /// Read-only access to registered tool records in this snapshot.
    pub fn tool(&self) -> ToolRepository<'_, Self> {
        ToolRepository::for_snapshot(self)
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
    transaction: &'a crate::StoreTx<'a>,
}

impl Tx<'_> {
    pub(crate) fn connection(&self) -> &Connection {
        self.transaction.connection()
    }

    // The task commit boundary still calls reservation and notice SQL that
    // use the legacy wrapper for this same transaction.
    pub(crate) fn legacy(&self) -> &crate::StoreTx<'_> {
        self.transaction
    }

    /// Read-write access to command audit-event records inside this transaction.
    pub fn audit_event(&self) -> AuditEventRepository<'_, Self> {
        AuditEventRepository::for_tx(self)
    }

    /// Read-write access to v2 workflow audit-envelope records inside this transaction.
    pub fn v2_audit(&self) -> V2AuditRepository<'_, Self> {
        V2AuditRepository::for_tx(self)
    }

    /// Read-write access to invocation trace and agent-execution records inside this transaction.
    pub fn invocation(&self) -> InvocationRepository<'_, Self> {
        InvocationRepository::for_tx(self)
    }

    /// Read-write access to host admission-load records inside this transaction.
    pub fn admission_load(&self) -> AdmissionLoadRepository<'_, Self> {
        AdmissionLoadRepository::for_tx(self)
    }

    /// Access host automation records in this write transaction.
    pub fn automation(&self) -> AutomationRepository<'_, Self> {
        AutomationRepository::for_tx(self)
    }

    /// Access host condition records in this write transaction.
    pub fn condition(&self) -> ConditionRepository<'_, Self> {
        ConditionRepository::for_tx(self)
    }

    /// Access host review records in this write transaction.
    pub fn review(&self) -> ReviewRepository<'_, Self> {
        ReviewRepository::for_tx(self)
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

    /// Access to workspace-partitioned friction records inside this transaction.
    pub fn friction(&self) -> FrictionRepository<'_, Self> {
        FrictionRepository::for_tx(self)
    }

    /// Access to host-local routine records inside this transaction.
    pub fn routine(&self) -> RoutineRepository<'_, Self> {
        RoutineRepository::for_tx(self)
    }

    /// Read-write access to fleet notice records inside this transaction.
    pub fn notice(&self) -> NoticeRepository<'_, Self> {
        NoticeRepository::for_tx(self)
    }
    /// Read-write access to job runs, their steps, pipeline states, task bindings
    /// and automation action keys inside this transaction.
    pub fn job_run(&self) -> JobRunRepository<'_, Self> {
        JobRunRepository::for_tx(self)
    }
    /// Read-write access to local pull admissions inside this transaction.
    pub fn local_pull(&self) -> LocalPullRepository<'_, Self> {
        LocalPullRepository::for_tx(self)
    }
    /// Read-write access to workspace drain intents inside this transaction.
    pub fn drain_intent(&self) -> DrainIntentRepository<'_, Self> {
        DrainIntentRepository::for_tx(self)
    }
    /// Read-write access to lease notices inside this transaction.
    pub fn lease_notice(&self) -> LeaseNoticeRepository<'_, Self> {
        LeaseNoticeRepository::for_tx(self)
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

    /// Read-write access to host-local installed-plugin records inside this transaction.
    pub fn plugin(&self) -> PluginRepository<'_, Self> {
        PluginRepository::for_tx(self)
    }

    /// Read-write access to host-local script grant records inside this transaction.
    pub fn script_grant(&self) -> ScriptGrantRepository<'_, Self> {
        ScriptGrantRepository::for_tx(self)
    }

    /// Read-write access to registered tool records inside this transaction.
    pub fn tool(&self) -> ToolRepository<'_, Self> {
        ToolRepository::for_tx(self)
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
                transaction: store_tx,
            };
            op(&tx)
        })
    }

    /// Append one audit row at `path` outside [`Store::write`].
    ///
    /// Named second write path for audit lines that cannot share an ordinary
    /// writable [`Store`] handle or must survive independently of an outer
    /// command: foreign-generation read-only readers (`is_write_free`) hold a
    /// read-only handle and must record CLI/tool telemetry without widening
    /// state access or running migrations, and checkout-rebind recovery appends
    /// its audit line directly without opening the full store. Opens a narrow
    /// `NOFOLLOW` connection, checks `current_schema_version == SUPPORTED_SCHEMA_VERSION`,
    /// and commits in its own `IMMEDIATE` transaction.
    pub fn append_audit_event_at_path(
        path: &Path,
        params: &AuditEventInsertParams,
        invocation: AuditInvocationFields<'_>,
    ) -> Result<(), OrbitError> {
        crate::repository::audit::event::append_at_path(path, params, invocation)
    }
}
