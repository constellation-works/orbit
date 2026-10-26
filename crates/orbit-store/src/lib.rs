#![deny(clippy::print_stderr, clippy::print_stdout)]
// Existing persistence APIs lack caller-facing docs, so this crate opts out of `missing_docs`.
#![allow(missing_docs)]
// Unit tests use unwrap/expect for fixture setup; production call sites remain linted.
#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used))]
//! Persistence contracts and storage composition for Orbit.
//!
//! File and SQLite drivers stay behind repositories and composition boundaries.

#[cfg(test)]
#[path = "../tests/store/support/git.rs"]
mod git_test_support;

pub mod compose;
pub mod contracts;
mod driver;
mod fs;
pub(crate) mod json_schema;
mod repository;
pub(crate) mod scope;
pub mod workflow;

/// Operator-only SQLite and coordination-registry access. Ordinary consumers
/// should depend on [`contracts`] and obtain implementations from composition.
pub mod maintenance {
    pub use crate::driver::sqlite::migration;
    pub mod task_registry {
        pub use crate::contracts::WorkspaceConfig;
        pub use crate::driver::file::workspace_binding::{
            read_workspace_config, read_workspace_config_optional, workspace_config_path,
            write_workspace_config,
        };
        pub use crate::driver::sqlite::task_registry::*;
    }
}

pub mod skill_store {
    pub use crate::driver::file::skill_store::*;
}

/// Friction record access and default tag-taxonomy initialization.
pub mod friction_store {
    pub use crate::driver::file::friction_store::ensure_default_tag_taxonomy;
    pub use crate::repository::friction::{FrictionAddParams, FrictionListFilter, FrictionStore};
}

use chrono::{DateTime, Utc};

pub use contracts::incident::{
    CauseBasis, DOCTOR_FINDINGS_MESSAGE_PREFIX, FailureClass, FailureIncident,
    FailureIncidentQuery, FailureIncidentReport, IncidentCause, IncidentEventRef,
    JOB_RUN_LIFECYCLE_LABEL, LIFECYCLE_DIAGNOSTIC_LABEL, PropagationLink,
    is_failure_only_diagnostic_surface,
};
pub(crate) use contracts::{
    ActiveTaskReservation, AuditActorAggregate, AuditAttributionAggregate, AuditRoleAggregate,
    ExpiredTaskReservation, ReleasedTaskReservation, TaskReservationCheckParams,
    TaskReservationCheckResult, TaskReservationListResult, TaskReservationOwnedConflictsParams,
    TaskReservationOwnedConflictsResult, TaskReservationRecord,
    TaskReservationReleaseByOwnerParams, TaskReservationReleaseByOwnerResult,
    TaskReservationReleaseParams, TaskReservationReleaseResult, TaskReservationReserveResult,
    TaskReservationScope, WorkspaceClaimAcquireParams, WorkspaceClaimAcquireResult,
    WorkspaceClaimCheckParams, WorkspaceClaimCheckResult, WorkspaceClaimHolder,
    WorkspaceClaimReleaseParams, WorkspaceClaimReleaseResult, WorkspaceClaimStatusResult,
};
pub use contracts::{
    ActivityInvocationCount, ActivityInvocationMetrics, AuditEventFilter, AuditEventInsertParams,
    AuditToolAggregate, InvocationInsertParams, InvocationQuery, InvocationRecord,
    InvocationRunCoverage, JobRunOutcomeFact, JobRunStepParams, RegisteredTaskResolution,
    RoutineFireIntentParams, RoutineFireRecord, RoutineFireState, SessionLogAppendParams,
    SessionLogEntry, SessionLogKind, TaskArtifactUpdateParams, TaskCompletionByComplexity,
    TaskCreateParams, TaskDefectFiling, TaskInvocationMetrics, TaskLockConflict, TaskLockHolder,
    TaskReservationReleaseReason, TaskReservationReserveParams, TaskStoreBackend,
    TaskTerminalTransition, ToolInvocationMetrics, V2AuditEventFilter, V2AuditEventInsertParams,
    V2AuditEventRow,
};
pub use driver::file::session_log_store::SessionLogStore;
pub use driver::file::task_bundle::bundle_io::is_unpublished_stub;
pub use driver::file::workspace_binding::workspace_id_for_orbit_dir;
pub use driver::sqlite::connection::Store;
pub(crate) use driver::sqlite::connection::StoreTx;
pub use driver::sqlite::routine_store::try_acquire_routine_sweep_lock;
pub use fs::lock::read_lock_holder;
pub use repository::task::{TaskCommitBoundary, admission_refusal};

/// Hooks for tests in crates that depend on `orbit-store`, behind the
/// `test-support` feature. Enable it only from `[dev-dependencies]`.
#[cfg(feature = "test-support")]
pub mod test_support {
    pub use crate::repository::task::coordination::fallback_hook::with_repair_owner_fallback_elapsed;
    pub use crate::repository::task::coordination::lock_timeout_hook::with_task_commit_lock_timeout;
}

pub(crate) fn parse_timestamp(raw: &str) -> rusqlite::Result<DateTime<Utc>> {
    let parsed = DateTime::parse_from_rfc3339(raw)
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
    Ok(parsed.with_timezone(&Utc))
}

pub(crate) fn now_string() -> String {
    Utc::now().to_rfc3339()
}
