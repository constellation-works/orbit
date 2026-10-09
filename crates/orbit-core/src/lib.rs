#![deny(clippy::print_stderr, clippy::print_stdout)]
// Legacy runtime command surfaces still need a focused documentation pass.
#![allow(missing_docs)]
// Unit tests use unwrap/expect for fixture setup; production call sites remain linted.
#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used))]
//! Directional application operations, runtime mechanisms, adapters,
//! bootstrap, and composition.
//!
//! This is the library crate that assembles all subsystems into the
//! [`OrbitRuntime`] — the single entry point used by the CLI, Web, and
//! `orbit-cmd` adapters. Composition loads resolved config and joins bootstrap,
//! application operations, adapters, and the runtime kernel.
//!
//! # Role
//! Depends on the lower Orbit crates (never on `orbit-cmd`). Consumed by
//! `orbit-cmd`, `orbit-cli`, and `orbit-web`; neutral
//! kernels below this layer do not import from `orbit-core`.
//!
//! Shared use cases live in [`application`]. Tool-host and engine-host protocol
//! translation lives in [`adapter`]. Runtime code owns mechanisms and imports
//! neither application nor adapter modules.
//!
//! # Root re-export policy (ORB-10016)
//! Every root `pub use` below is justified by a real import in a consumer
//! crate (`orbit-cli`, `orbit-web`, `orbit-cmd`). Anything else must be
//! imported from its owning module (`orbit_core::application::…`,
//! `orbit_core::runtime::…`) or its owning crate (`orbit_common`,
//! `orbit_store`, `orbit_engine`).
//!
//! # Key exports
//! - [`OrbitRuntime`] — fully initialized runtime; wraps stores, policy, tools, and event bus
//! - [`ActorIdentity`] — actor identity for audit trail attribution
//! - [`OrbitError`] — re-exported from `orbit-common::types` for CLI-layer convenience
//! - `application::*` — coordinated use cases and their DTOs
//! - `adapter::*` — command, tool-host, and engine-host protocol translation
//! - `skill_catalog` — re-exported skill store for CLI skill lookup
//!
//! # Dependency direction
//! orbit-common, orbit-store, orbit-policy, orbit-tools, orbit-search, orbit-engine
//! → `orbit-core` → orbit-cmd / orbit-web / orbit-cli

pub mod adapter;
pub mod application;
pub mod bootstrap;
pub mod composition;
pub mod context;
pub mod metrics;
mod paths;
pub mod runtime;

/// Allow this production Orbit binary to re-execute itself as a pipeline
/// worker. Call once from its `main` before it can submit any runs. Tests in
/// other crates must install the `test-support` worker override instead.
pub fn mark_process_as_pipeline_worker_binary() {
    application::job::pipeline::mark_process_as_pipeline_worker_binary();
}

/// Hooks for tests in crates that depend on `orbit-core`, behind the
/// `test-support` feature. Enable it only from `[dev-dependencies]`.
#[cfg(feature = "test-support")]
pub mod test_support {
    /// Replaced with the run id in every argv entry of a substitute worker.
    pub use crate::application::job::pipeline::worker_command_override::RUN_ID_PLACEHOLDER;
    /// Substitute the detached pipeline worker program for this whole test
    /// process. Any test that submits a pipeline run (ship, resume, auto, job)
    /// must install one: a test harness has no production entry-point marker,
    /// so an unsubstituted submission fails.
    pub use crate::application::job::pipeline::worker_command_override::install_process_wide as install_substitute_pipeline_worker;
    /// The review ledger requests that admit an attempt and record its
    /// reviewer's start and end, as the before-PR gate writes them, for tests
    /// that drive a reviewer without running the whole delivery pipeline.
    pub use orbit_store::contracts::{ReviewInvocationRecord, ReviewReserveRequest};

    use crate::application::routines::{
        RoutineMachineIdentity, RoutineWorkspaceProvider, SweepOptions, SweepOutcome,
    };

    /// One clock tick against an explicit global root at `now`, so a test can
    /// make a routine slot or an auto-task interval due.
    pub fn run_sweep_at(
        global_root: &std::path::Path,
        options: SweepOptions,
        local_machine: RoutineMachineIdentity,
        workspace_provider: &dyn RoutineWorkspaceProvider,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<SweepOutcome, crate::OrbitError> {
        crate::application::routines::sweep::run_sweep_at_with_providers_at(
            global_root,
            options,
            local_machine,
            workspace_provider,
            crate::runtime::host_signal::default_host_signal_probe().as_ref(),
            now,
        )
    }
}

// Store metric/scoreboard projections consumed by the dashboard's JSON API.
pub use orbit_store::scoreboard_summary;
pub use orbit_store::skill_store as skill_catalog;
pub use orbit_store::{InvocationInsertParams, InvocationQuery, InvocationRecord};

// Command-layer types the CLI names in its clap surfaces.
pub use application::distributed::{
    DrainEntryPoint, DrainEntryRefusal, PullSettlementEntry, WorkspacePullRequest,
};
pub use application::job::{
    AgentInvokeRequest, CatalogReferenceLayer, DrainAdmissionsStopRequest, DrainWorkerLimitRequest,
    PipelineInvokeResult, PipelineWaitEntry,
};
pub use application::routines::seed::{
    RoutineNameCollision, RoutineSeedIdentity, default_routine_name_collisions,
};
pub use application::search::{GlobalSearchHit, GlobalSearchKind, GlobalSearchParams};
pub use application::task::LockContentionReport;
pub use application::workflow::{CompletionPolicy, ShipMode, find_workflow, resolved_ship_mode};
pub use application::workspace_sync::{
    ManagedArtifactOutcome, ManagedArtifactScope, WorkspaceManagedArtifactSyncReport,
    reconcile_workspace_managed_artifacts,
};
pub use context::ActorIdentity;
pub use runtime::workspace::catalog::{FederatedWorkspaceTarget, WorkspaceCatalog, WorkspaceScope};
// Shared domain types (owned by orbit-common) that the CLI and dashboard
// render or construct.
pub use application::auto_tasks::{
    AutoTaskAddParams, AutoTaskDeleteParams, AutoTaskDeleteReport, AutoTaskUpdateParams,
};
pub use orbit_common::security::redaction::redact_sensitive_env_text;
pub use orbit_common::{NotFoundKind, OrbitError};
pub use orbit_store::{
    AuditEventFilter, AuditEventInsertParams, AuditToolAggregate, V2AuditEventFilter,
    V2AuditEventInsertParams,
};
pub use orbit_types::task::{
    DEFAULT_TASK_LIST_LIMIT, ExternalRef, Task, TaskComplexity, TaskCreateStatus, TaskPriority,
    TaskReferenceIndex, TaskStatus, TaskType, resolve_task_dependencies, resolve_task_relations,
    task_dependencies_ready_with_index,
};
pub use orbit_types::telemetry::{AuditEvent, AuditEventStatus, AuditStats};
pub use orbit_types::workflow::{
    AutoTaskDefinition, AutoTaskSchedule, AutoTaskTemplate, DedupePolicy, JobRun, JobRunState,
    JobRunStep, JobTargetType,
};
pub use orbit_types::workflow::{MissedRunPolicy, OverlapPolicy};
// Failure-incident grouping over the raw audit rows [ORB-10871]; consumed by
// the dashboard's incident, audit-summary, and scoreboard surfaces.
pub use orbit_store::{
    DOCTOR_FINDINGS_MESSAGE_PREFIX, FailureClass, FailureIncident, FailureIncidentQuery,
    FailureIncidentReport, IncidentEventRef, JOB_RUN_LIFECYCLE_LABEL, LIFECYCLE_DIAGNOSTIC_LABEL,
    PropagationLink, is_failure_only_diagnostic_surface,
};
// Routine fire records surfaced by the dashboard's routine-health JSON API.
pub use orbit_store::{RoutineFireRecord, RoutineFireState};
pub use runtime::engine::{ResolvedCrewProjection, TaskCrewRead};
pub use runtime::{OrbitRuntime, WorkspaceRuntimeBinding};
