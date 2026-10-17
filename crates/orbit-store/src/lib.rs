#![deny(clippy::print_stderr, clippy::print_stdout)]
// Legacy persistence surfaces still need a focused documentation pass.
#![allow(missing_docs)]
// Unit tests use unwrap/expect for fixture setup; production call sites remain linted.
#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used))]
//! One directional persistence crate for Orbit data.
//!
//! Consumer-visible traits and data live in [`contracts`]. Private file and
//! SQLite drivers never import one another; live invariants are joined by
//! repositories, one-shot migration/repair operations live in [`workflow`],
//! and [`compose`] is the construction boundary. Shared locking, path-safety,
//! atomic-write, and YAML mechanics live in narrowly named filesystem modules.
//!
//! # Role
//! Depends on `orbit-common` and `orbit-types`. Consumed by `orbit-core`, `orbit-engine`,
//! and `orbit-cmd`.
//!
//! # Key exports
//! - Backend trait types in [`contracts`]: [`contracts::TaskStoreBackend`],
//!   [`contracts::TaskDocumentStoreBackend`],
//!   [`contracts::TaskHistoryStoreBackend`],
//!   [`contracts::TaskArtifactStoreBackend`],
//!   [`contracts::TaskReservationStoreBackend`],
//!   [`contracts::JobRunStoreBackend`], [`contracts::AuditEventStoreBackend`],
//!   [`contracts::ToolStoreBackend`]
//! - Composition functions: `compose::workspace_task_backends`, `compose::workspace_job_run_store`,
//!   `global_executor_def_store`, `global_policy_def_store`,
//!   `audit_event_store_sqlite`, `workspace_coordinated_backends`, `tool_store_sqlite`
//! - [`SessionLogStore`] — lock-safe workspace session-log persistence
//! - [`Store`] — SQLite connection handle and transaction wrapper
//!
//! # Dependency direction
//! `orbit-common` / `orbit-types` ← `orbit-store` ← consumers such as orbit-core and orbit-engine

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

/// Friction records. Live reads and writes go through `FrictionStore`
/// (SQLite, ORB-10680); the tag taxonomy file did not move.
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
    TaskCreateParams, TaskInvocationMetrics, TaskLockConflict, TaskLockHolder,
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

pub(crate) fn parse_timestamp(raw: &str) -> rusqlite::Result<DateTime<Utc>> {
    let parsed = DateTime::parse_from_rfc3339(raw)
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
    Ok(parsed.with_timezone(&Utc))
}

pub(crate) fn now_string() -> String {
    Utc::now().to_rfc3339()
}
