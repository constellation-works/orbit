//! Bounded, operator-only hydration of a settled leaf's run evidence
//! [ORB-20906].
//!
//! A claim that settled before its follower sent the leaf's job, build and
//! failing step holds none of them, and the owner's run history never holds
//! the leaf. An operator on the owner can read the leaf's run record from its
//! follower, through that follower's authenticated operator route, and keep it
//! as the claim's immutable evidence with `hydrated` provenance.
//!
//! The route is a port ([`FollowerRunObserver`]): this crate never depends on a
//! transport, and the command that composes the existing federated client
//! supplies it. Hydration reads one follower run record per claim and, once per
//! drain, the admitting drain's. It cancels, resumes and retries nothing and
//! changes no task, claim or run; the only write is the evidence row.

use std::collections::BTreeMap;

use orbit_common::OrbitError;
use orbit_common::{NotFoundKind, RemoteToolCode};
use orbit_store::contracts::{LeafEvidenceWrite, LeafRootCause, LeafRunReport};
use orbit_types::workflow::{JobRunState, OrbitBuild};
use serde::Serialize;
use serde_json::Value;

use super::LeafSettlement;
use crate::OrbitRuntime;
use crate::application::contracts::PULL_DRAIN_JOB;

/// Most claims one hydration observes.
pub const MAX_LEAF_HYDRATION_LIMIT: usize = 100;
/// Claims one hydration observes when the operator names no limit.
pub const DEFAULT_LEAF_HYDRATION_LIMIT: usize = 25;

/// An authenticated operator read of one run record on a follower.
///
/// The implementation belongs to the composition that owns the transport. It
/// must be a read: `machine_id` names the follower, and the answer is the run
/// record as the follower's `orbit.workflow.run.show` reports it.
pub trait FollowerRunObserver {
    /// The run record `run_id` on the follower `machine_id`.
    fn observe_run(&self, machine_id: &str, run_id: &str) -> Result<Value, OrbitError>;
}

/// Why a claim was not hydrated.
type Refusal = (LeafHydrationOutcome, String);
/// The admitting drains already checked, by machine and drain run.
type DrainChecks = BTreeMap<(String, String), Result<(), Refusal>>;

/// Which settled claims to hydrate.
#[derive(Debug, Clone, Default)]
pub struct LeafHydrationRequest {
    /// Only this claim.
    pub claim_id: Option<String>,
    /// Only claims of this task.
    pub task_id: Option<String>,
    /// Only claims that ran on this machine, by id or name.
    pub machine: Option<String>,
    /// Only settlements recorded at or after this time.
    pub since: Option<chrono::DateTime<chrono::Utc>>,
    /// Observe claims that already hold evidence too, so an identical replay
    /// is confirmed and a differing one is reported as a conflict.
    pub recheck: bool,
    /// Most claims to observe; [`DEFAULT_LEAF_HYDRATION_LIMIT`] when `None`,
    /// at most [`MAX_LEAF_HYDRATION_LIMIT`].
    pub limit: Option<usize>,
}

/// What hydrating one claim came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LeafHydrationOutcome {
    /// The evidence was recorded.
    Hydrated,
    /// The claim already held the same evidence; nothing was written.
    Replayed,
    /// The claim holds different evidence and keeps it; nothing was written.
    Conflict,
    /// The follower could not be asked or did not answer.
    Unreachable,
    /// The follower answered that it holds no such run.
    Absent,
    /// The follower's record does not belong to this claim's tuple.
    Mismatch,
    /// The claim never bound a leaf run, so there is nothing to observe.
    NoLeafRun,
}

/// One claim's hydration result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LeafHydrationRow {
    pub task_id: String,
    pub claim_id: String,
    pub machine_id: String,
    pub leaf_run_id: Option<String>,
    pub outcome: LeafHydrationOutcome,
    /// Why, for every outcome but `hydrated` and `replayed`.
    pub detail: Option<String>,
}

/// A hydration's results.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LeafHydrationReport {
    pub rows: Vec<LeafHydrationRow>,
    /// Claims the request selected beyond the limit; run it again for them.
    pub remaining: usize,
}

impl OrbitRuntime {
    /// Read the selected settled claims' leaf run records from their followers
    /// through `observer` and keep what they say as immutable, `hydrated`
    /// evidence. Operator-only: a worker bound to a claim is refused, and the
    /// observer is the operator's own route.
    pub fn hydrate_leaf_run_evidence(
        &self,
        request: &LeafHydrationRequest,
        observer: &dyn FollowerRunObserver,
    ) -> Result<LeafHydrationReport, OrbitError> {
        if self.worker_invocation().is_some() {
            return Err(OrbitError::CapabilityRefused(
                "hydrating settlement evidence reads a follower's run records and is an \
                 operator action, not a claimed worker's"
                    .into(),
            ));
        }
        let limit = request
            .limit
            .unwrap_or(DEFAULT_LEAF_HYDRATION_LIMIT)
            .clamp(1, MAX_LEAF_HYDRATION_LIMIT);
        let owner_workspace = self.workspace_id()?;
        let owner_machine = self.automation_machine_identity().map(str::to_string);
        let mut selected = self
            .leaf_settlements(request.since, false)?
            .into_iter()
            .filter(|settlement| selects(request, settlement))
            .collect::<Vec<_>>();
        let remaining = selected.len().saturating_sub(limit);
        selected.truncate(limit);

        let mut drains = DrainChecks::new();
        let mut rows = Vec::new();
        for settlement in selected {
            let mut row = LeafHydrationRow {
                task_id: settlement.task_id.clone(),
                claim_id: settlement.claim_id.clone(),
                machine_id: settlement.machine_id.clone(),
                leaf_run_id: settlement.leaf_run_id.clone(),
                outcome: LeafHydrationOutcome::NoLeafRun,
                detail: Some("the claim never bound a leaf run".into()),
            };
            let Some(leaf) = settlement.leaf_run_id.as_deref() else {
                rows.push(row);
                continue;
            };
            let outcome = self.hydrate_one(
                &settlement,
                leaf,
                observer,
                &mut drains,
                &owner_workspace,
                owner_machine.as_deref(),
            )?;
            (row.outcome, row.detail) = match outcome {
                Ok(outcome) => (outcome, None),
                Err((outcome, detail)) => (outcome, Some(detail)),
            };
            rows.push(row);
        }
        Ok(LeafHydrationReport { rows, remaining })
    }

    /// The outcome for one claim: `Ok` when evidence was recorded or already
    /// held, else the refusal and why. A store failure is an error.
    fn hydrate_one(
        &self,
        settlement: &LeafSettlement,
        leaf: &str,
        observer: &dyn FollowerRunObserver,
        drains: &mut DrainChecks,
        owner_workspace: &str,
        owner_machine: Option<&str>,
    ) -> Result<Result<LeafHydrationOutcome, Refusal>, OrbitError> {
        let machine = settlement.machine_id.as_str();
        let observed = match observer.observe_run(machine, leaf) {
            Ok(value) => value,
            Err(error) => return Ok(Err(observation_failure(&error))),
        };
        let report = match leaf_report(&observed, settlement, leaf) {
            Ok(report) => report,
            Err(detail) => return Ok(Err((LeafHydrationOutcome::Mismatch, detail))),
        };
        let drain = drains
            .entry((machine.to_string(), settlement.drain_run_id.clone()))
            .or_insert_with(
                || match observer.observe_run(machine, &settlement.drain_run_id) {
                    Ok(value) => drain_belongs(
                        &value,
                        machine,
                        &settlement.drain_run_id,
                        owner_workspace,
                        owner_machine,
                    )
                    .map_err(|detail| (LeafHydrationOutcome::Mismatch, detail)),
                    Err(error) => Err(observation_failure(&error)),
                },
            );
        if let Err((outcome, detail)) = drain {
            return Ok(Err((*outcome, format!("admitting drain: {detail}"))));
        }
        let write = match self
            .stores()
            .tasks()
            .record_hydrated_leaf_evidence(&settlement.claim_id, report)
        {
            Ok(write) => write,
            Err(OrbitError::InvalidInput(message)) => {
                return Ok(Err((LeafHydrationOutcome::Mismatch, message)));
            }
            Err(error) => return Err(error),
        };
        Ok(match write {
            LeafEvidenceWrite::Recorded(_) => Ok(LeafHydrationOutcome::Hydrated),
            LeafEvidenceWrite::Replayed(_) => Ok(LeafHydrationOutcome::Replayed),
            LeafEvidenceWrite::Conflict(held) => Err((
                LeafHydrationOutcome::Conflict,
                format!(
                    "the claim already holds {} evidence recorded {} that differs from what \
                     the follower reports; the held evidence stands",
                    held.provenance.as_str(),
                    held.recorded_at
                ),
            )),
        })
    }
}

fn selects(request: &LeafHydrationRequest, settlement: &LeafSettlement) -> bool {
    request
        .claim_id
        .as_deref()
        .is_none_or(|claim| claim == settlement.claim_id)
        && request
            .task_id
            .as_deref()
            .is_none_or(|task| task == settlement.task_id)
        && request.machine.as_deref().is_none_or(|machine| {
            machine == settlement.machine_id || Some(machine) == settlement.machine_name.as_deref()
        })
        && (request.recheck || settlement.run_evidence.provenance.is_none())
}

/// `absent` only for the follower's own "no such run"; every other failure
/// leaves the question open and is `unreachable`.
fn observation_failure(error: &OrbitError) -> Refusal {
    let absent = matches!(
        error,
        OrbitError::NotFound {
            kind: NotFoundKind::JobRun,
            ..
        } | OrbitError::RemoteTool {
            code: RemoteToolCode::NotFound,
            ..
        }
    );
    (
        if absent {
            LeafHydrationOutcome::Absent
        } else {
            LeafHydrationOutcome::Unreachable
        },
        error.to_string(),
    )
}

/// The run object of a `workflow.run.show` answer, whether the answer is the
/// run or wraps it.
fn run_object(answer: &Value) -> &Value {
    answer
        .get("run")
        .filter(|run| run.get("run_id").is_some())
        .unwrap_or(answer)
}

fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn executed_machine(run: &Value) -> Option<&str> {
    run.get("executed_on")?
        .get("machine_id")?
        .as_str()
        .map(str::trim)
}

/// The leaf's evidence if the observed record is this claim's leaf: its own id,
/// the claim's machine (so a same-named run on another host never matches),
/// one of the claimed-leaf jobs, exactly the claim's task, and terminal.
fn leaf_report(
    answer: &Value,
    settlement: &LeafSettlement,
    leaf: &str,
) -> Result<LeafRunReport, String> {
    let run = run_object(answer);
    if text(run, "run_id") != Some(leaf) {
        return Err(format!(
            "the follower answered run {:?} for requested run {leaf}",
            text(run, "run_id")
        ));
    }
    if executed_machine(run) != Some(settlement.machine_id.as_str()) {
        return Err(format!(
            "run {leaf} executed on {:?}, not the claim's machine {}",
            executed_machine(run),
            settlement.machine_id
        ));
    }
    let job = text(run, "job_id").unwrap_or_default();
    if !crate::runtime::host_resource::delivery::CLAIMED_LEAF_JOBS.contains(&job) {
        return Err(format!("run {leaf} is job '{job}', not a claimed leaf"));
    }
    let tasks = run
        .get("task_ids")
        .and_then(Value::as_array)
        .map(|tasks| tasks.iter().filter_map(Value::as_str).collect::<Vec<_>>())
        .unwrap_or_default();
    if tasks != [settlement.task_id.as_str()] {
        return Err(format!(
            "run {leaf} delivers {tasks:?}, not the claim's task {}",
            settlement.task_id
        ));
    }
    let terminal = text(run, "state")
        .and_then(|state| serde_json::from_value::<JobRunState>(Value::String(state.into())).ok())
        .is_some_and(JobRunState::is_terminal);
    if !terminal {
        return Err(format!("run {leaf} is not terminal"));
    }
    let build = match run.get("build") {
        None | Some(Value::Null) => None,
        Some(build) => Some(
            serde_json::from_value::<OrbitBuild>(build.clone())
                .map_err(|error| format!("run {leaf} reports an unreadable build: {error}"))?,
        ),
    };
    Ok(LeafRunReport {
        run_id: leaf.to_string(),
        job_id: job.to_string(),
        build,
        root_cause: observed_root_cause(run),
    })
}

/// The failing step as the run record reports it: its top-level recorded
/// error, named by the step that recorded exactly that error.
fn observed_root_cause(run: &Value) -> Option<LeafRootCause> {
    let code = text(run, "error_code").map(str::to_string);
    let message = text(run, "error_message").map(str::to_string);
    if code.is_none() && message.is_none() {
        return None;
    }
    let step = run
        .get("steps")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .rev()
        .find(|step| {
            text(step, "error_code").map(str::to_string) == code
                && text(step, "error_message").map(str::to_string) == message
        })
        .and_then(|step| text(step, "target_id"))
        .map(str::to_string);
    Some(
        LeafRootCause {
            step,
            code,
            message,
        }
        .bounded(),
    )
}

/// The admitting drain's record names this claim's machine and this owner.
fn drain_belongs(
    answer: &Value,
    machine: &str,
    drain: &str,
    owner_workspace: &str,
    owner_machine: Option<&str>,
) -> Result<(), String> {
    let run = run_object(answer);
    if text(run, "run_id") != Some(drain) {
        return Err(format!(
            "the follower answered another run for drain {drain}"
        ));
    }
    if text(run, "job_id") != Some(PULL_DRAIN_JOB) {
        return Err(format!("run {drain} is not a pull drain"));
    }
    if executed_machine(run) != Some(machine) {
        return Err(format!(
            "drain {drain} did not run on the claim's machine {machine}"
        ));
    }
    let destination = run.get("input").and_then(|input| input.get("destination"));
    let names = |key: &str| destination.and_then(|destination| text(destination, key));
    if names("owner_workspace_id") != Some(owner_workspace) {
        return Err(format!("drain {drain} pulls from another owner workspace"));
    }
    if let Some(owner) = owner_machine
        && names("owner_machine_id") != Some(owner)
    {
        return Err(format!("drain {drain} pulls from another owner machine"));
    }
    Ok(())
}
