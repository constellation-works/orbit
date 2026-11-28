//! Run commands and their typed results. Only this module and its flows can
//! construct a command, pairing its operation with the work it authorizes.

mod auto;

mod catalog;

mod dashboard;

mod dashboard_drain;

mod detail;

mod drain;

mod failure;

mod fields;

mod fleet;

mod history;

mod inspection;

mod job;

mod leaves;

mod lineage;

mod locate;

mod lock_holders;

mod logs;

mod metrics;

mod pipeline;

mod read;

mod script;

mod show;

mod submission;

mod task_labels;

mod workflow;

use super::exec::{self, App, DashboardRow, Operation};
use crate::{Core, OrbitError};
use std::marker::PhantomData;

pub use catalog::{JobCatalog, JobRunsPage};
pub(crate) use drain::prepare_input;
pub(crate) use failure::failure_root_line;
pub use history::{FailureRow, RunHistory};
pub use job::{JobEntry, PilotRequest, SubmittedJob, WorkflowDispatchResult, wait_timeout};
pub use leaves::{DrainLeafSummary, WaitingBacklog, WaitingTask, waiting_backlog};
pub use locate::{LookupError, RunLookup, admit_dashboard_replay};
pub use logs::{LogFollower, LogSnapshot};
pub use metrics::{
    ActivityCount, ActivityRole, BucketGranularity, BucketOutcomeRow, DoubleReadSummary,
    JobOutcomeRow, JobRunReliability, KnowledgeStatsSummary, MIN_CONFIDENT_SAMPLE,
    ManagedExecutionCost, MetricsWindow, NormalizedTokenSummary, OrchestrationBucketKind,
    OrchestrationBucketSummary, OrchestrationModelSummary, OrchestrationSummary, OutcomeCounts,
    Rate, RatioSummary, RecoveryReliability, ReliabilityWindow, RuntimeReliability, TaskTokenSpend,
    TaskTokenTotals, TokenInputSummary,
};
pub use read::RunRead;
pub use script::DashboardScriptGrant;
pub use show::{RunDetail, ShownRun, WaitingTasks};

// Surface re-exports: what the CLI, dashboard and composition name from this subject.
pub use crate::app::workspace::workspace_live_run_ids;
pub use crate::application::change_watch::{RunStateStamp, TaskChangeStamp};
pub use crate::application::contracts::{
    ClaimedLeafStage, DrainClaimedLeaf, PULL_DRAIN_JOB, PullSettlementEntry, PullSettlementOutcome,
    SUPERVISOR_LOCAL_REVIEW_SCOPE,
};
pub use crate::application::distributed::{
    FollowerRunObserver, LeafHydrationOutcome, LeafHydrationRequest, PULL_START_CONFLICT_ARG_IDS,
    PULL_START_HOST_REQUIRES_ARG_ID, PullStartOptionsSupplied, WorkspacePullRequest,
    pull_start_review_refusal,
};
pub use crate::application::job::{
    ActivityInvocationEvidence, AutoDrainRequest, AutoDrainSubmissionProfile,
    AutoDrainSubmissionReceipt, CatalogReferenceLayer, DrainAdmissionsStopOutcome,
    DrainAdmissionsStopRequest, DrainWindowExtensionOutcome, DrainWindowExtensionRequest,
    DrainWorkerLimitChangeProfile, DrainWorkerLimitOutcome, DrainWorkerLimitRequest,
    EscalationTriageSubmitError, JobCatalogEntry, JobCatalogFilter, JobFailureStreak,
    JobRunCancelOutcome, JobRunCancelResultProfile, JobRunHolder, JobRunListParams, JobRunOrder,
    JobRunProjectionEvidence, JobRunProjectionProfile, PipelineInvokeResult, PipelineWaitEntry,
    PipelineWorkerLogSnapshot, ResumeSubmissionProfile, ResumeSubmissionReceipt, RunDisplayStep,
    RunFailureCause, RunStepSource, ShipRunRequest, ShipSubmissionProfile, ShipSubmissionReceipt,
    TaskAutoFixError, TaskCrewBackfill, agent_invoke_result,
    display_steps_with_agent_execution_to_json, fold_run_incidents,
    job_catalog_to_json_with_last_run, job_run_task_ids, project_job_run, run_error_step,
    run_upgrade_paused,
};
pub use crate::application::workflow::{CompletionPolicy, ShipMode, resolved_ship_mode};
pub use orbit_runtime::HostLifetime;
pub use orbit_runtime::audit::run::{
    AuditStepOutcome, RunAuditEvent, RunAuditStep, RunProviderProcess, RunScriptExecution,
};
pub use orbit_runtime::audit::run_events::{RunEventsPage, RunEventsPageResult};
pub use orbit_runtime::audit::run_logs::{HTTP_PREVIEW_MAX_BYTES, RunCliInvocationProfile};
pub use orbit_runtime::host_resource::accuracy::{
    AccuracySelection, DrainLimiters, DrainShadowDecisions, ErrorSummary, ForecastAccuracy,
    OffsetAccuracy,
};
pub use orbit_runtime::host_resource::forecast::HostCapacity;
pub use orbit_runtime::host_resource::profile::{
    BUILD_LIKE_CORES, BUILD_LIKE_MINUTES, ResourceProfile,
};
pub use orbit_runtime::host_resource::{HostResourceMonitor, HostResourceStatus};
pub use orbit_runtime::scripts::{ScriptGrantOptions, ScriptGrantState, ScriptStatus};
pub use orbit_store::contract::LandedDiffStats;

/// A run operation whose result has type `T`.
pub struct Command<'a, T> {
    operation: Operation,
    flow: Box<RunFlow<'a, T>>,
}

impl<'a, T> Command<'a, T> {
    fn new(operation: Operation, flow: impl FnOnce(&Core) -> Result<T, Error> + 'a) -> Self {
        Self {
            operation,
            flow: Box::new(move |core| {
                flow(core).map(|value| Outcome {
                    value,
                    exit_code: 0,
                    row: RowDue::Declared,
                })
            }),
        }
    }
}

type RunFlow<'a, T> = dyn FnOnce(&Core) -> Result<Outcome<T>, OrbitError> + 'a;

/// The read model or submission receipt, with its native command exit status.
pub struct Outcome<T> {
    /// The value the surface renders.
    pub value: T,
    exit_code: i32,
    row: RowDue,
}

/// What the dashboard row a command declared records of an admitted outcome.
enum RowDue {
    /// A success.
    Declared,
    /// Nothing: the command wrote nothing.
    Nothing,
    /// A failure with this text.
    Failure(&'static str),
}

/// A run flow's error, preserved for surface-specific error rendering.
pub type Error = OrbitError;

struct RunApp<'a, T>(PhantomData<&'a T>);

impl<'a, T: 'a> App for RunApp<'a, T> {
    type Command = Command<'a, T>;
    type Outcome = Outcome<T>;
    type Error = Error;

    fn operation(command: &Self::Command) -> Operation {
        command.operation.clone()
    }

    fn run(core: &Core, command: Self::Command) -> Result<Self::Outcome, Error> {
        (command.flow)(core)
    }

    fn exit_code(outcome: &Self::Outcome) -> i32 {
        outcome.exit_code
    }

    fn dashboard_row(
        result: &Result<Self::Outcome, Error>,
        _target: &str,
        arguments: serde_json::Value,
    ) -> Option<DashboardRow> {
        match result.as_ref().map(|outcome| &outcome.row) {
            Ok(RowDue::Nothing) => None,
            Ok(RowDue::Failure(failure)) => Some(DashboardRow {
                failure: Some((*failure).to_string()),
                target: None,
                arguments,
            }),
            Ok(RowDue::Declared) | Err(_) => Some(DashboardRow::of(result, arguments)),
        }
    }
}

/// Authorize, audit and execute one run command.
pub fn exec<'a, T: 'a>(core: &Core, command: Command<'a, T>) -> Result<Outcome<T>, Error> {
    exec::run::<RunApp<'a, T>>(core, command)
}

/// Allow this production Orbit binary to re-execute itself as a pipeline
/// worker. Call once from its `main` before it can submit any runs. Tests in
/// other crates must install the `test-support` worker override instead.
pub fn mark_process_as_pipeline_worker_binary() {
    crate::application::job::pipeline::mark_process_as_pipeline_worker_binary();
}
