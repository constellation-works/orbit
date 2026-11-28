//! Directional application operations, adapters, bootstrap, and composition
//! over the `orbit-runtime` kernel.
//!
//! This crate assembles all subsystems into the `OrbitRuntime` and owns the
//! use cases. Composition loads resolved config and joins bootstrap,
//! application operations, adapters, and the runtime kernel.
//!
//! # Public face
//! The crate exports three things: [`app`], one module per subject; [`Core`],
//! the opaque handle composition returns for one opened workspace; and
//! [`Error`]. A surface builds an app's `Command`, passes it with its `Core`
//! to that app's `exec`, which runs through the chokepoint that authorizes and
//! audits it, and renders the `Outcome`. A type or function a surface or
//! `orbit-cmd` names is re-exported from the app that owns its subject.
//!
//! Shared use cases live in `unit` and `application`, protocol translation in
//! `adapter`, and the runtime kernel is the `orbit-runtime` crate, which
//! `runtime` re-exports inside this crate beside the `OrbitRuntime` that owns
//! it. The kernel never calls an application method: a mechanism that needs
//! a use case's answer takes it as an argument, and a flow that needs both
//! lives here. These modules are crate-private in a shipped build.
//!
//! # Dependency direction
//! orbit-common, orbit-store, orbit-policy, orbit-tools, orbit-search, orbit-engine
//! → orbit-runtime → `orbit-core` → orbit-cmd / orbit-web / orbit-cli

#![deny(clippy::print_stderr, clippy::print_stdout)]
// Legacy runtime command surfaces still need a focused documentation pass.
#![allow(missing_docs)]
// Unit tests use unwrap/expect for fixture setup; production call sites remain linted.
#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used))]
#![cfg_attr(
    not(any(test, feature = "test-support")),
    deny(clippy::disallowed_methods)
)]

// A bare `cargo test` from a managed-run shell must match one from a clean shell.
#[cfg(test)]
orbit_common::isolate_test_process!();

// `adapter`, `application`, `bootstrap`, `composition` and `runtime` are
// crate-private in a shipped build and public under the dev-only
// `test-support` feature, so integration fixtures can seed and inspect them.
// A shipped build has no such consumer, so an item only a fixture reaches is
// unused there; a `test-support` build still reports a dead crate-private
// item.
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub mod adapter;

#[cfg(not(feature = "test-support"))]
#[allow(dead_code, unused_imports)]
pub(crate) mod adapter;
pub mod app;

#[cfg(feature = "test-support")]
#[doc(hidden)]
pub mod application;

#[cfg(not(feature = "test-support"))]
#[allow(dead_code, unused_imports)]
pub(crate) mod application;

#[cfg(feature = "test-support")]
#[doc(hidden)]
pub mod bootstrap;

#[cfg(not(feature = "test-support"))]
#[allow(dead_code, unused_imports)]
pub(crate) mod bootstrap;

#[cfg(feature = "test-support")]
#[doc(hidden)]
pub mod composition;

#[cfg(not(feature = "test-support"))]
#[allow(dead_code, unused_imports)]
pub(crate) mod composition;

#[path = "metrics/mod.rs"]
pub(crate) mod metric;

#[cfg(feature = "test-support")]
#[doc(hidden)]
pub mod runtime;

#[cfg(not(feature = "test-support"))]
#[allow(dead_code, unused_imports)]
pub(crate) mod runtime;

#[cfg(feature = "test-support")]
#[doc(hidden)]
pub mod unit;

#[cfg(not(feature = "test-support"))]
pub(crate) mod unit;

/// Hooks for tests in crates that depend on `orbit-core`, behind the
/// `test-support` feature. Enable it only from `[dev-dependencies]`.
#[cfg(feature = "test-support")]
pub mod test_support {
    /// Review ledger operations for delivery fixtures that drive a reviewer directly.
    pub mod review_ledger {
        pub use crate::application::review::ledger::{
            import_claimed_ledger, open_reconciliation, record_certificate, record_invocation,
            release, reserve, review_reset, settle, update_reconciliation,
        };
    }
    /// Checkpoint operations for CLI lifecycle fixtures.
    pub mod automation_checkpoint {
        pub use orbit_automation::checkpoint::{commit_state, initialize, stall};
    }

    /// Adapter implementing `orbit-engine`'s host traits over a borrowed
    /// [`FixtureRuntime`], for fixtures that drive engine entry points or host
    /// traits directly.
    pub use crate::adapter::engine_host::EngineHost;
    pub use crate::app::auto_task::completion_test_support::with_completion_adapter_run;
    pub use crate::app::notice::{with_notice_close_snapshot_hook, with_notice_post_time};
    pub use crate::app::resident::{
        with_resident_action_close_failure, with_resident_cycle_admission_checkpoint,
    };
    /// Freeze source budget time in an isolated test process and its CLI children.
    pub use crate::application::automation::source::SOURCE_ELAPSED_ENV;
    pub use crate::application::automation::source::test_support::with_delivery_source_clock;
    pub use crate::application::automation::with_reset_probe_hook;
    pub use crate::application::distributed::admission_checkpoint::{
        with_claim_settle_checkpoint, with_pull_admission_checkpoint,
    };
    pub use crate::application::job::crew_pools::{
        with_crew_backfill_checkpoint, with_crew_draw_quantile, with_crew_selection_checkpoint,
        without_auto_effort_exploration,
    };
    #[cfg(unix)]
    pub use crate::application::job::pipeline::with_pipeline_worker_log_open_hook;
    /// Replaced with the run id in every argv entry of a substitute worker.
    pub use crate::application::job::pipeline::worker_command_override::RUN_ID_PLACEHOLDER;
    /// Substitute the detached pipeline worker program for this whole test
    /// process. Any test that submits a pipeline run (ship, resume, auto, job)
    /// must install one: a test harness has no production entry-point marker,
    /// so an unsubstituted submission fails.
    pub use crate::application::job::pipeline::worker_command_override::install_process_wide as install_substitute_pipeline_worker;
    /// Stop a worker's startup observer around reaping and exit recording.
    pub use crate::application::job::pipeline::{WorkerReapPhase, install_worker_reap_hook};
    pub use crate::application::job::pipeline::{
        pipeline_worker_observer_completed, pipeline_workers_spawned,
    };
    pub use crate::application::job::with_run_root_cause_write_hook;
    pub use crate::application::review::fulfilment_test_support::with_review_evidence_completion;
    pub use crate::application::task::candidate_checkpoint::install_candidate_selection_checkpoint;
    pub use crate::application::task::{
        with_decision_answer_checkpoint, with_decision_dkim_resolver,
        with_decision_raise_checkpoint,
    };
    /// Fail this thread's next plugin-manifest digest record as a store error.
    pub use crate::application::tool_name_migration::plugin_manifest_publish_hook::fail_next_plugin_digest_upsert;
    /// Fail this thread's next rollback after a plugin digest record fails.
    pub use crate::application::tool_name_migration::plugin_manifest_publish_hook::fail_next_plugin_manifest_restore;
    /// Run a competing plugin migration before this thread publishes a manifest.
    pub use crate::application::tool_name_migration::plugin_manifest_publish_hook::with_plugin_manifest_publish_hook;
    pub use crate::runtime::{
        with_host_load_sampling_wait, with_owner_read_retry_wait, with_pipeline_poll_wait,
    };
    /// Replace the backlog forecast's wall-time budget for a scenario with
    /// arrivals, in milliseconds, or its generated-node bound, in an isolated
    /// test's server child.
    pub use crate::unit::backlog_forecast::{
        COMPUTE_BUDGET_ENV as BACKLOG_FORECAST_COMPUTE_BUDGET_ENV,
        MAX_GENERATED_ENV as BACKLOG_FORECAST_MAX_GENERATED_ENV,
    };
    pub use crate::unit::landing::with_landing_retry_reservation_hook;
    pub use crate::unit::managed_asset::with_auto_task_manifest_read_hook;
    pub use crate::unit::pull_drain::{
        deliver_recorded_pull_settlements, pending_pull_settlements, settle_orphaned_pulls,
    };
    /// Fail the next drain pass's release of its unadmitted gap-fit reservations.
    pub use crate::unit::workspace_auto::with_reservation_release_failure;
    /// The engine crate, for a fixture that drives an engine mechanism directly.
    pub use orbit_engine as fixture_engine;
    pub use orbit_engine::test_support::with_cli_supervision_clock;
    /// The store crate, for a fixture that seeds or inspects rows directly.
    pub use orbit_store as fixture_store;
    /// The review ledger requests that admit an attempt and record its
    /// reviewer's start and end, as the before-PR gate writes them, for tests
    /// that drive a reviewer without running the whole delivery pipeline.
    pub use orbit_store::V2AuditEventInsertParams;
    pub use orbit_store::contract::{ReviewInvocationRecord, ReviewReserveRequest};
    /// Canonical (artifact-hashing) task-bundle reads on this thread.
    pub use orbit_store::test_support::canonical_bundle_reads;
    /// Full job-run step hydrations (`read_steps` / `read_steps_for_runs`) on
    /// this thread.
    pub use orbit_store::test_support::job_run_full_step_reads;
    /// `JobRun` rows decoded on this thread.
    pub use orbit_store::test_support::job_run_rows_hydrated;
    /// Lightweight task-bundle reads on this thread, listing or by id.
    pub use orbit_store::test_support::lightweight_bundle_reads;
    /// Listing-path task-bundle reads on this thread.
    pub use orbit_store::test_support::listing_bundle_reads;
    /// Treat every pending landing repair's owner fallback window as passed
    /// on this thread, for tests of the owner drain taking a repair.
    pub use orbit_store::test_support::with_repair_owner_fallback_elapsed;
    /// Shorten the task-commit lock deadline of the boundary sections entered
    /// on this thread, for tests of what follows a lock timeout.
    pub use orbit_store::test_support::with_task_commit_lock_timeout;

    use crate::app::routine::{
        RoutineMachineIdentity, RoutineWorkspaceProvider, SweepOptions, SweepOutcome,
    };

    /// The workspace handle a fixture seeds and inspects directly, beneath
    /// the `Core` its server is given.
    pub type FixtureRuntime = crate::OrbitRuntime;

    /// The `Core` a service opens over a fixture's workspace handle. It
    /// shares the handle's stores, so the fixture sees what the server writes.
    pub fn fixture_core(runtime: &FixtureRuntime) -> crate::Core {
        crate::composition::core_for_service(runtime.clone())
    }

    /// The workspace handle beneath `core`, for a fixture's assertions.
    pub fn fixture_runtime(core: &crate::Core) -> &FixtureRuntime {
        core.runtime()
    }

    /// Open a runtime over `global_root` and the workspace `.orbit` directory
    /// `workspace_root`, for a fixture outside `orbit-core`.
    pub fn runtime_from_roots(
        global_root: &std::path::Path,
        workspace_root: &std::path::Path,
    ) -> Result<crate::OrbitRuntime, crate::OrbitError> {
        crate::OrbitRuntime::from_roots(global_root, workspace_root)
    }

    /// Supply a different caller snapshot while keeping the fixture's runtime state.
    pub fn with_runtime_env(
        runtime: crate::OrbitRuntime,
        env: orbit_common::Env,
    ) -> crate::OrbitRuntime {
        crate::OrbitRuntime::from_kernel(runtime.into_kernel().with_test_env(env))
    }

    /// Serve the production broker dispatch in an isolated test whose peer is the test process.
    #[cfg(unix)]
    pub fn start_test_run_broker(
        runtime: &crate::OrbitRuntime,
        run: orbit_engine::PluginBrokerRun,
    ) -> Result<crate::runtime::plugin::broker::PluginBroker, orbit_common::OrbitError> {
        use crate::runtime::plugin::broker::{PeerAnchor, PluginBroker};
        let peer = orbit_common::process::ancestry::process_start_key(std::process::id())
            .ok_or_else(|| orbit_common::OrbitError::Execution("test process has exited".into()))?;
        let run_id = run.run_id.clone();
        let dispatch = crate::adapter::command::RunDispatch::new(runtime.clone(), run);
        let broker = PluginBroker::start(
            &runtime.global_root(),
            &run_id,
            std::sync::Arc::new(dispatch),
        )?;
        broker.bind_anchor(PeerAnchor::Ancestor(peer));
        Ok(broker)
    }

    /// A claimed leaf's call to its owner on the host's own channel, the one
    /// the review gate writes its records on. The owner refuses the reserved
    /// review names on every other claimed-worker route, so a test that plays
    /// the gate must cross here and keep the agent's writes on the agent
    /// route.
    pub fn route_worker_host_tool(
        runtime: &crate::OrbitRuntime,
        name: &str,
        input: serde_json::Value,
    ) -> Result<serde_json::Value, crate::OrbitError> {
        runtime.route_worker_host_tool(name, input)
    }

    /// One clock tick against an explicit global root at `now`, so a test can
    /// make a routine slot or an auto-task interval due.
    pub fn run_sweep_at(
        global_root: &std::path::Path,
        options: SweepOptions,
        local_machine: RoutineMachineIdentity,
        workspace_provider: &dyn RoutineWorkspaceProvider,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<SweepOutcome, crate::OrbitError> {
        crate::app::routine::run_sweep_pass(
            crate::composition::store_opener(),
            crate::app::routine::SweepContext {
                now_utc: now,
                ..crate::app::routine::SweepContext::production(global_root, workspace_provider)
            },
            options,
            local_machine,
        )
    }
}

/// The opaque handle a surface holds for one opened workspace.
pub use composition::Core;
/// What an app's `exec` returns when a command fails.
pub use orbit_common::OrbitError as Error;

// Crate-private shorthands the modules below name as `crate::…`.
pub(crate) use app::routine::RoutineSeedIdentity;
pub(crate) use app::search::{GlobalSearchKind, SearchRequest, SearchRequestProfile};
pub(crate) use app::workspace::reconcile_workspace_managed_artifacts;
pub(crate) use application::contracts::DrainEntryPoint;
pub(crate) use application::distributed::WorkspacePullRequest;
pub(crate) use application::job::{
    AutoDrainRequest, CatalogReferenceLayer, DrainAdmissionsStopRequest,
    DrainWindowExtensionRequest, DrainWorkerLimitRequest, PipelineInvokeResult, PipelineWaitEntry,
    PipelineWaitStatus, ShipModeResolutionProfile, ShipRunRequest,
};
pub(crate) use application::workflow::{
    CompletionPolicy, ShipMode, find_workflow, resolved_ship_mode,
};
pub(crate) use orbit_common::security::redaction::redact_sensitive_env_text;
pub(crate) use orbit_common::{NotFoundKind, OrbitError};
pub(crate) use orbit_runtime::context::ActorIdentity;
pub(crate) use orbit_store::V2AuditEventFilter;
pub(crate) use orbit_store::skill_store as skill_catalog;
pub(crate) use orbit_types::task::{DEFAULT_TASK_LIST_LIMIT, Task, TaskStatus};
pub(crate) use orbit_types::telemetry::incident::FailureIncidentReport;
pub(crate) use orbit_types::workflow::{JobRun, JobRunState, JobRunStep};
pub(crate) use runtime::OrbitRuntime;
pub(crate) use runtime::engine::ResolvedCrewProjection;
pub(crate) use unit::backlog_forecast::BacklogForecastHistory;
pub(crate) use unit::workspace_auto::{FleetReadinessFacts, IDLE_WITH_WORK_DAYS, IdleWithWork};

use orbit_runtime::paths;

#[cfg(test)]
#[path = "../tests/support/admission.rs"]
mod admission_test_support;

#[cfg(test)]
#[path = "../tests/support/git.rs"]
mod git_test_support;

#[cfg(test)]
#[path = "../tests/support/task_params.rs"]
mod task_params_test_support;
