#![deny(clippy::print_stderr, clippy::print_stdout)]
// Legacy persistence surfaces still need a focused documentation pass.
#![allow(missing_docs)]
// Unit tests use unwrap/expect for fixture setup; production call sites remain linted.
#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used))]
#![allow(
    rustdoc::broken_intra_doc_links,
    rustdoc::invalid_html_tags,
    rustdoc::private_intra_doc_links
)]
//! One directional persistence crate for Orbit data.
//!
//! Consumer-visible traits and data live in [`contracts`]. Private file and
//! SQLite drivers never import one another; live invariants are joined by
//! repositories, one-shot migration/repair operations live in [`workflow`],
//! and [`compose`] is the construction boundary. Shared locking, path-safety,
//! atomic-write, and YAML mechanics live in narrowly named filesystem modules.
//!
//! # Role
//! Depends only on `orbit-common`. Consumed by `orbit-core`, `orbit-engine`,
//! and `orbit-cmd`.
//!
//! # Key exports
//! - Backend trait types: [`TaskStoreBackend`], [`TaskDocumentStoreBackend`],
//!   [`TaskHistoryStoreBackend`],
//!   [`TaskArtifactStoreBackend`], [`TaskReservationStoreBackend`],
//!   [`JobRunStoreBackend`], [`AuditEventStoreBackend`], [`ToolStoreBackend`]
//! - Composition functions: `compose::workspace_task_backends`, `compose::workspace_job_run_store`,
//!   `global_executor_def_store`, `global_policy_def_store`,
//!   `audit_event_store_sqlite`, `workspace_coordinated_backends`, `tool_store_sqlite`
//! - [`SessionLogStore`] — lock-safe workspace session-log persistence
//! - [`Store`] / [`StoreTx`] — SQLite connection handle and transaction wrapper
//! - [`validate_instance_against_schema`] — JSON Schema validation for activity I/O
//!
//! # Dependency direction
//! `orbit-common` ← `orbit-store` ← consumers such as orbit-core and orbit-engine

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
            workspace_id_for_orbit_dir, write_workspace_config,
        };
        pub use crate::driver::sqlite::task_registry::*;
    }
}

/// Live JSON state-file operations used by the tool protocol adapter.
pub use driver::file::run_state as state_io;

pub mod skill_store {
    pub use crate::driver::file::skill_store::*;
}

/// Friction records. Live reads and writes go through [`FrictionStore`]
/// (SQLite, ORB-10680); the file-layout helpers re-exported here own the hub
/// publication decision and the legacy tree kept as read-only evidence.
pub mod friction_store {
    pub use orbit_types::identity::validate_friction_id;

    pub use crate::driver::file::friction_store::{
        canonical_hub_friction_root, ensure_default_tag_taxonomy, prepare_hub_friction_root,
        readable_hub_friction_root,
    };
    pub use crate::repository::friction::{
        FrictionAddParams, FrictionListFilter, FrictionReportedCount, FrictionStore,
        FrictionUpdateParams, StoredFrictionRecord,
    };
    pub use crate::workflow::friction::{
        FrictionImportReport, export_workspace_frictions, import_workspace_frictions,
    };
}

pub mod pr_scoreboard {
    pub use crate::driver::file::scoreboard::pr_scoreboard::{
        record_pr_count_with_revision, record_pr_count_without_revision,
    };
}

pub mod scoreboard_summary {
    pub use crate::driver::file::scoreboard::scoreboard_summary::{
        AgentSummary, CoverageAvailability, CoverageNote, FrictionSummary, NormalizedTokenSummary,
        NotableCompletion, NotableCompletions, ORCHESTRATION_SCHEMA_VERSION,
        OrchestrationBucketKind, OrchestrationBucketSummary, OrchestrationModelSummary,
        OrchestrationSummary, PrSummary, RecentSummary, ScoreboardCoverage, ScoreboardInputs,
        ScoreboardSummary, ScoreboardWindow, TokenSummary, TopToolCall, WorkflowRunCount,
        generate_summary, generate_summary_with_audit_tool_calls, generate_summary_with_inputs,
        summary_path, write_summary,
    };
}

pub mod token_scoreboard {
    pub use crate::repository::token_scoreboard::write_token_scoreboard;
}

use chrono::{DateTime, Utc};

pub use contracts::incident::{
    CASCADE_WINDOW_SECS, DEFAULT_SCAN_LIMIT as FAILURE_INCIDENT_SCAN_LIMIT, FailureClass,
    FailureIncident, FailureIncidentQuery, FailureIncidentReport, IncidentEventRef,
    JOB_RUN_LIFECYCLE_LABEL, LIFECYCLE_DIAGNOSTIC_LABEL, PropagationLink,
    build_report as build_failure_incident_report, classify as classify_failure,
    group_failure_incidents, is_failure_only_diagnostic_surface, is_lifecycle_diagnostic,
    normalize_message as normalize_failure_message, signature_for as failure_signature_for,
};
pub use contracts::{
    ActiveTaskReservation, ActivityInvocationCount, ActivityInvocationMetrics,
    AgentInvocationMetrics, AuditActorAggregate, AuditAttributionAggregate, AuditEventFilter,
    AuditEventInsertParams, AuditEventStoreBackend, AuditInvocationFields, AuditRoleAggregate,
    AuditToolAggregate, AuditToolCallCountsByRole, AuditToolCallCountsBySurfaceAndRole,
    AuditTopToolCall, BoundedFacts, ExecutorDefStoreBackend, ExpiredTaskReservation,
    FrictionStoreBackend, InvocationAccountingFact, InvocationAccountingQuery,
    InvocationInsertParams, InvocationQuery, InvocationRecord, InvocationRunCoverage,
    InvocationStoreBackend, InvocationToolCallRecord, JobRunOutcomeFact, JobRunQuery,
    JobRunStepParams, JobRunStoreBackend, PolicyDefStoreBackend, RegisteredTaskResolution,
    ReleasedTaskReservation, RoutineCursor, RoutineFireIntentParams, RoutineFireRecord,
    RoutineFireState, RoutinePauseRecord, RoutineStoreBackend, SessionLogAppendParams,
    SessionLogEntry, SessionLogFilter, SessionLogKind, SessionLogStoreBackend,
    TaskArtifactStoreBackend, TaskArtifactUpdateParams, TaskCompletionByComplexity,
    TaskCoordinationCommit, TaskCoordinationCommitOutcome, TaskCoordinationCommitParams,
    TaskCoordinationRow, TaskCreateParams, TaskDocumentStoreBackend, TaskDocumentUpdateParams,
    TaskHistoryStoreBackend, TaskHistoryUpdateParams, TaskInvocationMetrics, TaskLockConflict,
    TaskLockHolder, TaskReservationCheckParams, TaskReservationCheckResult,
    TaskReservationListResult, TaskReservationOwnedConflictsParams,
    TaskReservationOwnedConflictsResult, TaskReservationReleaseByOwnerParams,
    TaskReservationReleaseByOwnerResult, TaskReservationReleaseParams,
    TaskReservationReleaseReason, TaskReservationReleaseResult, TaskReservationReserveParams,
    TaskReservationReserveResult, TaskReservationScope, TaskReservationStoreBackend,
    TaskStoreBackend, ToolInvocationMetrics, ToolStoreBackend, V2AuditEventFilter,
    V2AuditEventInsertParams, V2AuditEventRow, V2AuditStoreBackend, WorkspaceClaimAcquireParams,
    WorkspaceClaimAcquireResult, WorkspaceClaimCheckParams, WorkspaceClaimCheckResult,
    WorkspaceClaimHolder, WorkspaceClaimReleaseParams, WorkspaceClaimReleaseResult,
    WorkspaceClaimStatusResult,
};
pub use driver::file::session_log_store::SessionLogStore;
pub use driver::file::task_bundle::bundle_io::is_unpublished_stub;
pub use driver::file::workspace_binding::{
    read_workspace_config, read_workspace_config_optional, workspace_config_path,
    workspace_id_for_orbit_dir, write_workspace_config,
};
pub use driver::sqlite::connection::{Store, StoreTx};
pub use driver::sqlite::routine_store::{RoutineSweepLock, try_acquire_routine_sweep_lock};
pub use fs::lock::{LockHolderInfo, read_lock_holder};
pub use json_schema::{validate_instance_against_schema, validate_schema_document};
pub use repository::task::{TaskCommitBoundary, admission_refusal};

pub(crate) fn parse_timestamp(raw: &str) -> rusqlite::Result<DateTime<Utc>> {
    let parsed = DateTime::parse_from_rfc3339(raw)
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
    Ok(parsed.with_timezone(&Utc))
}

pub(crate) fn now_string() -> String {
    Utc::now().to_rfc3339()
}
