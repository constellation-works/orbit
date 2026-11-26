//! Persistence contracts and storage composition for Orbit.
//!
//! File and SQLite drivers stay behind repositories and composition boundaries.

#![deny(clippy::print_stderr, clippy::print_stdout)]
// Existing persistence APIs lack caller-facing docs, so this crate opts out of `missing_docs`.
#![allow(missing_docs)]
// Unit tests use unwrap/expect for fixture setup; production call sites remain linted.
#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used))]
#![cfg_attr(
    not(any(test, feature = "test-support")),
    deny(clippy::disallowed_methods)
)]

mod boundary;
pub mod compose;
pub mod contract;
mod driver;
mod file_store;
mod fs;
pub(crate) mod json_schema;
pub mod migrate;
mod repository;
mod store;
/// Operator-only SQLite and coordination-registry access. Ordinary consumers
/// should depend on [`contract`] and obtain implementations from composition.
pub mod maintenance {
    pub mod task_registry {
        pub use crate::contract::WorkspaceConfig;
        pub use crate::driver::file::workspace_binding::{
            read_workspace_config, read_workspace_config_optional, workspace_config_path,
            write_workspace_config,
        };
        pub use crate::repository::task_index::*;
    }

    pub use crate::driver::sqlite::host::schema as migration;
}
pub mod skill_store {
    pub use crate::driver::file::skill_store::*;
    pub use crate::repository::skill::SkillRepository;
}
/// Workspace configuration for friction tags.
pub mod friction_taxonomy {
    pub use crate::repository::friction::taxonomy::{
        ensure_default_tag_taxonomy, load_tag_taxonomy, load_tag_taxonomy_with_descriptions,
    };
}
/// Host-only recovery authority persistence (`authority.db`).
pub mod recovery_authority {
    pub use crate::driver::sqlite::authority::FILE_NAME;
    pub use crate::repository::recovery_authority::{
        AttemptCertificateRow, RecoveryAttemptRecord, RecoveryAuthorityReadOnly,
        RecoveryAuthorityStore, RecoveryAuthorityTx, RecoveryCertificateRecord,
    };
}
/// Hooks for tests in crates that depend on `orbit-store`, behind the
/// `test-support` feature. Enable it only from `[dev-dependencies]`.
#[cfg(feature = "test-support")]
pub mod test_support {
    pub use crate::boundary::coordination::fallback_hook::with_repair_owner_fallback_elapsed;
    pub use crate::boundary::coordination::lock_timeout_hook::with_task_commit_lock_timeout;
    pub use crate::boundary::coordination::task_write_hold_hook::with_task_commit_write_hold;
    pub use crate::repository::recovery_authority::seed_legacy_authority_certificate;

    /// Canonical (artifact-hashing) task-bundle reads on this thread.
    pub fn canonical_bundle_reads() -> u64 {
        crate::boundary::v2_bundle::CANONICAL_BUNDLE_READS.with(std::cell::Cell::get)
    }

    /// Lightweight task-bundle reads on this thread, listing or by id.
    pub fn lightweight_bundle_reads() -> u64 {
        crate::boundary::v2_bundle::LIGHTWEIGHT_BUNDLE_READS.with(std::cell::Cell::get)
    }

    /// Listing-path task-bundle reads on this thread.
    pub fn listing_bundle_reads() -> u64 {
        crate::boundary::v2_bundle::LISTING_BUNDLE_READS.with(std::cell::Cell::get)
    }
}

pub use boundary::TaskV2Store;
pub use boundary::{AdmissionCall, TaskCommitBoundary, TaskCoordination, admission_refusal};
use chrono::{DateTime, Utc};
pub use contract::incident::{
    CauseBasis, DOCTOR_FINDINGS_MESSAGE_PREFIX, FailureClass, FailureIncident,
    FailureIncidentQuery, FailureIncidentReport, IncidentCause, IncidentEventRef,
    JOB_RUN_LIFECYCLE_LABEL, LIFECYCLE_DIAGNOSTIC_LABEL, PropagationLink,
};
pub(crate) use contract::{
    ActiveTaskReservation, ExpiredTaskReservation, ReleasedTaskReservation,
    TaskReservationCheckParams, TaskReservationCheckResult, TaskReservationListResult,
    TaskReservationOwnedConflictsParams, TaskReservationOwnedConflictsResult,
    TaskReservationRecord, TaskReservationReleaseByOwnerParams,
    TaskReservationReleaseByOwnerResult, TaskReservationReleaseParams,
    TaskReservationReleaseResult, TaskReservationReserveResult, TaskReservationScope,
    WorkspaceClaimAcquireParams, WorkspaceClaimAcquireResult, WorkspaceClaimCheckParams,
    WorkspaceClaimCheckResult, WorkspaceClaimHolder, WorkspaceClaimReleaseParams,
    WorkspaceClaimReleaseResult, WorkspaceClaimStatusResult,
};
pub use contract::{
    ActivityInvocationCount, ActivityInvocationMetrics, AuditEventFilter, AuditEventInsertParams,
    AuditToolAggregate, AuditToolModelAggregate, InvocationInsertParams, InvocationQuery,
    InvocationRecord, InvocationRunCoverage, JobRunOutcomeFact, JobRunStepParams,
    RegisteredTaskResolution, RoutineFireIntentParams, RoutineFireRecord, RoutineFireState,
    SessionLogAppendParams, SessionLogEntry, SessionLogKind, TaskArtifactUpdateParams,
    TaskCompletionByComplexity, TaskCreateParams, TaskDefectFiling, TaskInvocationMetrics,
    TaskLockConflict, TaskLockHolder, TaskReservationReleaseReason, TaskReservationReserveParams,
    TaskTerminalTransition, ToolInvocationMetrics, V2AuditEventFilter, V2AuditEventInsertParams,
    V2AuditEventRow,
};
pub use driver::file::executor_def_store::ExecutorDefDir;
pub use driver::file::policy_def_store::PolicyDefDirs;
pub use driver::file::session_log_store::SessionLogDir;
pub use driver::file::task_bundle::bundle_io::is_unpublished_stub;
pub use driver::file::workspace_binding::workspace_id_for_orbit_dir;
pub use driver::sqlite::host::connection::Store;
pub(crate) use driver::sqlite::host::connection::StoreTx;
pub use file_store::{FileSnapshot, FileStore, FileTx};
pub use fs::lock::read_lock_holder;
pub use fs::routine_sweep_lock::try_acquire_routine_sweep_lock;
pub use repository::admission_load::AdmissionLoadRepository;
pub use repository::application_consent::ApplicationConsentRepository;
pub use repository::audit::{AuditEventRepository, V2AuditRepository};
pub use repository::automation::AutomationRepository;
pub use repository::backlog_forecast::BacklogForecastRepository;
pub use repository::codebase_fact::{AgentFactRepository, CodebaseFactRepository};
pub use repository::condition::ConditionRepository;
pub use repository::crew_memory::CrewMemoryRepository;
pub use repository::drain_intent::DrainIntentRepository;
pub use repository::evolution::EvolutionRepository;
pub use repository::executor_def::ExecutorDefRepository;
pub use repository::experiment::{ExperimentAssignmentRepository, ExperimentRepository};
pub use repository::incident::IncidentRepository;
pub use repository::invocation::InvocationRepository;
pub use repository::job_run::JobRunRepository;
pub use repository::landed_diff::LandedDiffRepository;
pub use repository::lease_notice::LeaseNoticeRepository;
pub use repository::local_pull::LocalPullRepository;
pub use repository::notice::NoticeRepository;
pub use repository::plugin::PluginRepository;
pub use repository::policy_def::PolicyDefRepository;
pub use repository::provider_limit::ProviderLimitRepository;
pub use repository::reliability::ReliabilityRepository;
pub use repository::resource_usage::{ResourceUsageRepository, WorkerResourceLimitRepository};
pub use repository::retention::{RetentionRepository, visit_blob_reference_text};
pub use repository::review::ReviewRepository;
pub use repository::script_grant::ScriptGrantRepository;
pub use repository::session_log::SessionLogRepository;
pub use repository::skill::SkillRepository;
pub use repository::tool::ToolRepository;
pub use repository::{
    TaskArtifactRepository, TaskDocumentRepository, TaskHistoryRepository,
    TaskReservationRepository,
};
pub use store::{Snapshot, Tx};

#[cfg(test)]
#[path = "../tests/store/support/git.rs"]
mod git_test_support;

pub(crate) fn parse_timestamp(raw: &str) -> rusqlite::Result<DateTime<Utc>> {
    let parsed = DateTime::parse_from_rfc3339(raw)
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
    Ok(parsed.with_timezone(&Utc))
}

pub(crate) fn now_string() -> String {
    Utc::now().to_rfc3339()
}
