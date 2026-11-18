use crate::condition::GuardVerdict;
use chrono::DateTime;
use chrono::Utc;
use orbit_common::OrbitError;
use orbit_types::workflow::JobRunState;
use std::path::Path;
use std::time::Instant;

use super::super::breaker::ObservedFire;
use super::super::loader::{LoadedRoutine, RoutineLoadError};

/// Core-supplied owner facts, independent of persisted run status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOwnerLiveness {
    Alive,
    Stopped,
    Unknown,
}

/// Dispatch seam for the sweep. The production impl
/// (in Core) wraps one `OrbitRuntime` per source workspace and
/// dispatches through `submit_pipeline_run` / `show_job_run`; tests supply a
/// fake so the sweep's fire / retry / overlap / outcome-sync orchestration is
/// exercised deterministically without spawning pipeline workers.
pub trait RoutineDispatch {
    fn evaluate_delivery(
        &self,
        _routine: &LoadedRoutine,
        _dry_run: bool,
        _now: DateTime<Utc>,
    ) -> Result<orbit_types::workflow::automation::AutomationDiagnostic, OrbitError> {
        Err(OrbitError::Execution(
            "delivery source adapter unavailable".into(),
        ))
    }

    /// One pass of a condition routine's firing protocol. Hosts without
    /// condition support refuse, and nothing fires.
    fn evaluate_condition(
        &self,
        _routine: &LoadedRoutine,
        _dry_run: bool,
        _now: DateTime<Utc>,
    ) -> Result<crate::condition::ConditionDiagnostic, OrbitError> {
        Err(OrbitError::Execution(
            "condition evaluation unavailable on this host".into(),
        ))
    }

    /// Evaluate the trigger's `when:` guard at a due slot. A host that cannot
    /// evaluate a configured guard holds the slot: unknown fails closed.
    fn evaluate_guard(&self, routine: &LoadedRoutine, _now: DateTime<Utc>) -> GuardVerdict {
        match routine.definition.trigger.when {
            Some(_) => GuardVerdict::unknown("guard evaluation unavailable on this host"),
            None => GuardVerdict::Open,
        }
    }

    /// Submit `job_name` in the source workspace rooted at `source_orbit_dir`
    /// under `actor`, returning the dispatched run id. `slot` is the RFC 3339
    /// scheduled slot consumed by this fire [ORB-12255].
    fn submit(
        &self,
        source_orbit_dir: &Path,
        job_name: &str,
        actor: &str,
        slot: &str,
    ) -> Result<String, OrbitError>;

    /// Current run state for a dispatched fire, when the run is queryable.
    fn run_state(&self, source_orbit_dir: &Path, run_id: &str) -> Option<JobRunState>;

    /// [ORB-10597] Whether the run's recorded owner process is still executing,
    /// asked independently of its persisted state. Consulted only for runs
    /// marked `interrupted`, which carries no teardown and so is not evidence
    /// that the work stopped.
    fn run_owner_liveness(&self, source_orbit_dir: &Path, run_id: &str) -> RunOwnerLiveness;

    /// Newest runs a state-, delivery- or condition-triggered routine's
    /// automation admitted, newest first, as the circuit breaker observes
    /// them. Hosts that cannot list them report none.
    fn routine_runs(
        &self,
        _routine: &LoadedRoutine,
        _limit: usize,
    ) -> Result<Vec<ObservedFire>, OrbitError> {
        Ok(Vec::new())
    }

    /// Error class of a failed run: its failed step's recorded failure code,
    /// else its terminal state. `None` when the run cannot be read.
    fn run_failure_class(&self, _source_orbit_dir: &Path, _run_id: &str) -> Option<String> {
        None
    }

    /// Identity of the Orbit build running this sweep. A breaker tripped
    /// under another build admits a trial at once.
    fn host_orbit_build(&self) -> Option<String> {
        None
    }
}

/// Options for one sweep pass.
#[derive(Debug, Clone, Copy)]
pub struct SweepOptions {
    /// Report what would fire without dispatching runs or minting tasks. Host-local
    /// housekeeping (checkout validation, the scheduler store) still runs.
    pub dry_run: bool,
    /// Cadence of the host clock that invokes this pass. The production
    /// assembly supplies the configured clock cadence; deterministic callers
    /// use the compatible 60-second default.
    pub sweep_cadence_seconds: u64,
    /// Shared monotonic deadline for all phases. `None` starts a fresh budget
    /// at the sweep boundary; a running operation finishes before yielding.
    pub deadline: Option<Instant>,
}

impl Default for SweepOptions {
    fn default() -> Self {
        Self {
            dry_run: false,
            sweep_cadence_seconds: 60,
            deadline: None,
        }
    }
}

/// Per-routine outcome of one sweep pass.
#[derive(Debug, Clone)]
pub struct RoutineSweepReport {
    /// Routine name.
    pub routine: String,
    /// Source workspace name.
    pub source: String,
    /// Whether the definition is `committed` or `local` origin.
    pub origin: &'static str,
    /// One of: `fired`, `retry_fired`, `would_fire`, `baselined`,
    /// `would_baseline`, `skipped`, `error`.
    pub action: &'static str,
    /// Why, for `skipped`/`error` rows.
    pub reason: Option<String>,
    /// Scheduled slot consumed (RFC 3339, UTC), when a fire was involved.
    pub slot: Option<String>,
    /// Run id returned by dispatch, when one was submitted.
    pub run_id: Option<String>,
    /// Members of the batch a state routine would admit, is admitting, or has
    /// in flight, each with why it is there [ORB-12746]. Empty otherwise.
    pub batch: Vec<orbit_types::workflow::automation::members::BatchMember>,
}

/// Per-auto-task outcome included in the host tick report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoTaskSweepReport {
    /// Auto-task definition name, or the failing file name for a load error.
    pub name: String,
    /// Source workspace name.
    pub source: String,
    /// One of: `minted`, `would_fire`, `baselined`, `would_baseline`,
    /// `skipped`, `error`.
    pub action: &'static str,
    /// Why, for `skipped` and `error` rows.
    pub reason: Option<String>,
    /// Scheduled slot consumed (RFC 3339, UTC), when a fire was involved.
    pub slot: Option<String>,
    /// Task minted by this tick.
    pub task_id: Option<String>,
}

/// A task settled from its merged delivery PR during one sweep pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalMergeSweepReport {
    /// The settled task.
    pub task_id: String,
    /// Source workspace name.
    pub source: String,
    /// The merged delivery PR number.
    pub pr_number: u64,
}

/// Result of one sweep pass.
#[derive(Debug, Default)]
pub struct SweepOutcome {
    /// Display name of the machine this pass ran on.
    pub machine_name: String,
    /// Stable identity of the machine this pass ran on.
    pub machine_id: String,
    /// True when another sweep held the lock and this pass exited early.
    pub lock_busy: bool,
    /// The tick exhausted its cooperative budget; callers must exit nonzero.
    pub deadline_exceeded: bool,
    /// Workspaces with work deferred to the next tick by the deadline.
    pub skipped_workspaces: Vec<String>,
    /// Per-routine outcomes.
    pub reports: Vec<RoutineSweepReport>,
    /// Per-auto-task outcomes, evaluated after all routine rows.
    pub auto_task_reports: Vec<AutoTaskSweepReport>,
    /// Fail-closed definition/load failures (those routines were absent).
    pub load_errors: Vec<RoutineLoadError>,
    /// `execution.env.pass` names this pass exported from the clock
    /// environment file into the runs it starts. Names only.
    pub clock_env_loaded: Vec<String>,
    /// Set when every discovered workspace failed to open, so this pass
    /// loaded nothing. The CLI prints this one row and exits non-zero.
    /// Partial load errors leave this `None`.
    pub no_workspace_loaded: Option<String>,
    /// Tasks settled from merged delivery PRs on this pass (owner only).
    pub external_merge_settlements: Vec<ExternalMergeSweepReport>,
}
