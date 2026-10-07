//! Submitting a follower's pull drain: `orbit run auto --pull <selector>`
//! [ORB-13625].
//!
//! Everything a pull drain will trust is established here, before a run
//! exists, from facts this host and the owner report — never from the
//! operator's word alone:
//!
//! - this checkout is a **replica**, and the selector names **its** owner
//!   machine and **its** logical workspace;
//! - the owner answers the read-only probe **as that machine**, and would
//!   admit this executor now (binary, protocol schema, ship mode), and this
//!   host can resolve the before-PR reviewer crew the owner's ship contract
//!   captured, when `review.before_pr` is on there.
//!
//! An empty `workflow.required_validation_commands` is not a refusal: its
//! claimed leaves run no required check, as an owner's own delivery does, and
//! `orbit run auto` notes the empty list.
//!
//! The resolved destination is persisted on the run, so every iteration, leaf
//! and retry addresses the same owner and workspace. A destination that later
//! stops answering is reported by the drain, never replaced by a local store.
//!
//! So is an operator's `--allow-crew` restriction [ORB-14174], by canonical
//! registry name after each name is checked against this host's crews: every
//! pass, and a resumed run, declares only those crews to the owner. It narrows
//! the implementation crews a claim may carry; the owner's before-PR reviewer
//! is checked on its own, and no configuration or task crew changes.
//!
//! A drain submitted without a window (`for_seconds` zero) is authorized for
//! exactly one admission pass, which its run state records as consumed before
//! that pass sends a request ([`PullSinglePass`]).

use orbit_common::OrbitError;
use orbit_store::contracts::PullDestination;
use orbit_types::workflow::{DrainAdmissionPass, JobRunTrigger, PullSinglePass, ResourceThrottle};
use serde_json::{Value, json};

use super::{ensure_distributed_mutation_available, probe_pull_contract};
use crate::application::job::PipelineInvokeResult;

/// The job a follower drain runs.
pub const PULL_DRAIN_JOB: &str = "workspace_pull_pipeline";

const PASS_FAILURE_THRESHOLD: u32 = 3;

/// Operator inputs for one follower drain.
#[derive(Debug, Clone, Default)]
pub struct WorkspacePullRequest<'a> {
    /// Host-qualified owner selector, copied from federated discovery.
    pub selector: &'a str,
    /// The admission window. `None` or zero is one admission pass.
    pub for_seconds: Option<u64>,
    pub max_active_leaf_runs: Option<u32>,
    /// Crews this window may run claimed work as (`--allow-crew`). Empty
    /// means every crew the window can run.
    pub allowed_crews: &'a [String],
    pub actor: Option<&'a str>,
}

impl crate::OrbitRuntime {
    /// Persist pass health atomically with the existing admission observation.
    /// Degradation latches for the window, so a successful settlement-only pass
    /// cannot hide the failure that stopped admissions.
    pub(crate) fn record_pull_pass(
        &self,
        run_id: &str,
        resource_throttle: Option<ResourceThrottle>,
        error: Option<&str>,
        error_code: Option<&str>,
    ) -> Result<DrainAdmissionPass, OrbitError> {
        let mut recorded = None;
        self.stores()
            .jobs()
            .update_run_state(run_id, &mut |_, state| {
                let previous = state.drain_last_pass.as_ref();
                let was_degraded = previous.is_some_and(|pass| pass.degraded);
                let previous_count = previous.map_or(0, |pass| pass.consecutive_pass_failures);
                let consecutive_pass_failures = if error.is_some() {
                    previous_count.saturating_add(1)
                } else if was_degraded {
                    previous_count
                } else {
                    0
                };
                let pass = DrainAdmissionPass {
                    capacity: None,
                    last_pass_error_code: error_code.map(ToOwned::to_owned).or_else(|| {
                        was_degraded
                            .then(|| previous.and_then(|pass| pass.last_pass_error_code.clone()))
                            .flatten()
                    }),
                    recorded_at: chrono::Utc::now(),
                    queued: 0,
                    deferred: Vec::new(),
                    excluded: Vec::new(),
                    excluded_total: 0,
                    resource_throttle: resource_throttle.clone(),
                    last_pass_error: error.map(ToOwned::to_owned).or_else(|| {
                        was_degraded
                            .then(|| previous.and_then(|pass| pass.last_pass_error.clone()))
                            .flatten()
                    }),
                    consecutive_pass_failures,
                    degraded: was_degraded
                        || error_code == Some("protocol_skew")
                        || consecutive_pass_failures >= PASS_FAILURE_THRESHOLD,
                };
                state.drain_last_pass = Some(pass.clone());
                recorded = Some(pass);
                Ok(())
            })?;
        recorded.ok_or_else(|| OrbitError::Store("pull drain pass was not recorded".into()))
    }

    /// Take the single admission pass of a drain submitted without a window
    /// [ORB-14174]: `true` exactly once per run lineage, recorded before the
    /// caller requests anything. A stop or graceful cancel already recorded
    /// takes it away, in the same transaction that would consume it, so
    /// neither can race the pass open. A state that cannot be read or written
    /// is an error: the caller must not admit without the record. Retry runs
    /// are never fresh authorizations, including legacy zero-window runs
    /// whose checkpoint predates `pull_single_pass`.
    pub(crate) fn take_pull_single_pass(&self, run_id: &str) -> Result<bool, OrbitError> {
        // Resume and replay links are immutable run metadata. Check them
        // before the state transaction so a legacy checkpoint with no marker
        // cannot be mistaken for a newly submitted zero-window drain.
        let retry_run = self
            .stores()
            .jobs()
            .get_job_run(run_id)?
            .is_some_and(|run| run.retry_source_run_id.is_some());
        let mut taken = false;
        let update = self
            .stores()
            .jobs()
            .update_run_state(run_id, &mut |_, state| {
                taken = !retry_run
                    && state.pull_single_pass.is_none()
                    && state.drain_admissions_stop.is_none()
                    && state.drain_cancel.is_none();
                if taken {
                    state.pull_single_pass = Some(PullSinglePass {
                        consumed_at: chrono::Utc::now(),
                    });
                }
                Ok(())
            })?;
        if update != orbit_types::workflow::RunStateUpdate::Updated {
            return Err(OrbitError::Store(format!(
                "pull drain {run_id} has no run state to record its single admission pass in"
            )));
        }
        Ok(taken)
    }

    /// Verify this replica against its owner and submit its pull drain.
    pub fn submit_workspace_pull_run(
        &self,
        request: WorkspacePullRequest<'_>,
        trigger: JobRunTrigger,
    ) -> Result<PipelineInvokeResult, OrbitError> {
        ensure_distributed_mutation_available("orbit.workflow.auto --pull")?;
        // Operator input that needs no owner is checked before the probe, so a
        // typo costs no network round trip.
        if request.max_active_leaf_runs == Some(0) {
            return Err(OrbitError::InvalidInput(
                "--concurrency must be at least 1".into(),
            ));
        }
        // Unknown or blank names fail here, before the probe and before any
        // run exists; canonical names are what every pass reads back.
        let allowed_crews = self.canonical_allowed_crews(request.allowed_crews)?;
        let destination = self.resolve_pull_destination(request.selector)?;
        let mut input = json!({
            "for_seconds": request.for_seconds.unwrap_or(0),
            "destination": serde_json::to_value(&destination)
                .map_err(|error| OrbitError::Store(error.to_string()))?,
        });
        if let Some(ceiling) = request.max_active_leaf_runs {
            input["max_active_leaf_runs"] = json!(ceiling);
        }
        if !allowed_crews.is_empty() {
            input[crate::runtime::engine::crew::ALLOWED_CREWS_INPUT_KEY] = json!(allowed_crews);
        }
        self.submit_pipeline_run_with_trigger(PULL_DRAIN_JOB, input, None, request.actor, trigger)
    }

    /// The owner this replica pulls from, checked against the replica's own
    /// registration and the owner's live answer.
    fn resolve_pull_destination(&self, selector: &str) -> Result<PullDestination, OrbitError> {
        let selector = selector.trim();
        let owner_machine = self.coordination_write_owner().ok_or_else(|| {
            OrbitError::CapabilityRefused(
                "`--pull` runs on a replica checkout; this checkout is its workspace's owner, \
                 whose drain is plain `orbit run auto`"
                    .into(),
            )
        })?;
        let (selector_machine, selector_workspace) = selector.split_once('/').ok_or_else(|| {
            OrbitError::UnknownSelector(format!(
                "'{selector}' is not a host-qualified selector; copy `selector` from federated \
                 orbit.workspace.list"
            ))
        })?;
        if selector_machine != owner_machine {
            return Err(OrbitError::InvalidInput(format!(
                "selector names machine '{selector_machine}', but this replica's owner is \
                 '{owner_machine}'"
            )));
        }
        let logical = self
            .workspace_runtime_binding()
            .map(|binding| binding.logical_workspace_id.as_str())
            .ok_or_else(|| {
                OrbitError::InvalidInput("this checkout is not a registered workspace".into())
            })?;
        if selector_workspace != logical {
            return Err(OrbitError::InvalidInput(format!(
                "selector names workspace '{selector_workspace}', but this replica is registered \
                 as '{logical}'"
            )));
        }
        let execution_machine = self
            .automation_machine_identity()
            .ok_or_else(|| {
                OrbitError::InvalidInput(
                    "this host has no registered machine identity; run `orbit init` first".into(),
                )
            })?
            .to_string();
        let transport = self.drain_owner_transport().ok_or_else(|| {
            OrbitError::InvalidInput(
                "this runtime has no federated owner route; register the owner with \
                 `orbit host add <ssh-target>`"
                    .into(),
            )
        })?;
        let report =
            probe_pull_contract(transport.as_ref(), selector, self.local_review_before_pr())
                .map_err(|error| match error {
                    // The selector parsed and names this replica's own owner, so
                    // the only thing missing is a route to that machine.
                    OrbitError::UnknownSelector(token) => OrbitError::UnknownSelector(format!(
                        "{token}: this host has no route to owner machine '{owner_machine}'; \
                     register it with `orbit host add <ssh-target>` and check the \
                     selector against federated orbit.workspace.list"
                    )),
                    other => other,
                })?;
        let answered_as = report.get("owner_machine_id").and_then(Value::as_str);
        if answered_as != Some(owner_machine) {
            return Err(OrbitError::InvalidInput(format!(
                "the owner answered as {answered_as:?}, not this replica's owner '{owner_machine}'"
            )));
        }
        if report.get("admits").and_then(Value::as_bool) != Some(true) {
            let diagnostics = report
                .get("diagnostics")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join("; ")
                })
                .unwrap_or_default();
            return Err(OrbitError::CapabilityRefused(format!(
                "the owner would refuse this executor ({}): {diagnostics}",
                report
                    .get("refusal")
                    .and_then(Value::as_str)
                    .unwrap_or("refused")
            )));
        }
        // The probe already judged the owner side; the reviewer crew it
        // captured must also run here [ORB-13908].
        if let Some(ship) = report
            .get("ship")
            .cloned()
            .and_then(|ship| serde_json::from_value(ship).ok())
            && let Some(refusal) = self.claimed_review_refusal(&ship)
        {
            return Err(OrbitError::CapabilityRefused(format!(
                "this executor cannot run the owner's before-PR review: {refusal}"
            )));
        }
        let owner_workspace_id = report
            .get("workspace_id")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .ok_or_else(|| OrbitError::Store("owner probe reported no workspace id".into()))?
            .to_string();
        Ok(PullDestination {
            owner_machine_id: owner_machine.to_string(),
            owner_workspace_id,
            selector: selector.to_string(),
            execution_machine_id: execution_machine,
        })
    }
}
