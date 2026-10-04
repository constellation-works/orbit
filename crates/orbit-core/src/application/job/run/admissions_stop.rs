//! Stop new admissions on this workspace's live auto drain [ORB-11283].
//!
//! Cancellation of the coordinator cannot implement this: auto children are
//! detached so they outlive the parent step, but cancelling a parent still
//! terminalizes *that* run, and a blocking dispatch would cascade. This
//! control writes a flag the admission path reads. The coordinator keeps its
//! run id, deadline, completion authorization, and already-dispatched
//! children. Those children keep running under the authority they were
//! admitted with.
//!
//! Stopping already-running workers is a separate, explicit
//! `orbit run cancel <child-run-id> --confirm` of each child, or a stop with
//! `force`, which also cancels each live drain the way
//! `orbit run cancel <drain> --force` does.

use orbit_common::OrbitError;
use orbit_common::observability::audit_id::audit_execution_id;
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::workflow::{JobRun, JobRunState, PipelineState, RunStateUpdate};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::workflow::{AUTO_WORKFLOW_ALIAS, find_workflow};

const STOP_REQUEST_AUDIT: &str = "pipeline.run.admissions_stop.requested";
const STOP_COMPLETION_AUDIT: &str = "pipeline.run.admissions_stop.completed";
const STOP_OPERATION: &str = "orbit.workflow.auto";

/// One operator request to stop this workspace's auto admissions.
#[derive(Debug, Clone, Copy)]
pub struct DrainAdmissionsStopRequest<'a> {
    pub actor: &'a str,
    pub source: &'a str,
    pub reason: Option<&'a str>,
    pub claim_token: Option<&'a str>,
    /// Also cancel each live drain and stop the work it has in flight: a
    /// pull drain's claimed leaves, whose claims go back to the owner's
    /// backlog, or a local drain's detached children.
    pub force: bool,
}

/// A child this coordinator still has in flight after admissions stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemainingDrainChild {
    pub run_id: String,
    pub job_name: String,
    pub phase: String,
    pub child_status: Option<String>,
}

/// Outcome for one auto coordinator this workspace owns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrainAdmissionsStopChange {
    pub run_id: String,
    pub job_id: String,
    /// `stopped` when the flag was newly written, `unchanged` when it was
    /// already set, `cancelled_queued` when a never-started coordinator was
    /// cancelled so it cannot admit later, `force_cancelled` when a forced
    /// stop cancelled it and stopped its in-flight work.
    pub outcome: &'static str,
    pub remaining_children: Vec<RemainingDrainChild>,
    /// Runs a forced stop cancelled under this coordinator.
    pub forced_runs: Vec<String>,
    /// Claimed leaves a forced stop could not confirm it stopped; each keeps
    /// its claim on the owner.
    pub unstopped_leaves: Vec<super::types::UnstoppedLeaf>,
}

/// Workspace-scoped result of `orbit run auto --stop`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrainAdmissionsStopResult {
    /// `idle` when this workspace has no auto coordinator, `stopped` when at
    /// least one running coordinator was acknowledged, `cancelled_queued`
    /// when only queued never-started coordinators were removed,
    /// `force_cancelled` when a forced stop cancelled at least one.
    pub outcome: &'static str,
    pub coordinators: Vec<DrainAdmissionsStopChange>,
    /// [ORB-13663] Pull settlements the stop's settle-only pass carried, for
    /// every owner this replica pulls from: recorded settlements delivered,
    /// and the unlaunched admissions of drains that are no longer running
    /// ended. Empty on a workspace that never pulled.
    pub pull_settlements: Vec<crate::application::distributed::PullSettlementEntry>,
    /// The owner machine when this checkout is a replica, so a report can say
    /// "no pull drain" rather than "no auto coordinator" there.
    pub replica_owner_machine_id: Option<String>,
}

impl OrbitRuntime {
    /// Stop new admissions for every live auto coordinator in this workspace.
    ///
    /// The selected workspace is the runtime's own: other workspaces and other
    /// jobs are not inspected. A missing coordinator is success (`idle`). A
    /// repeated stop is success (`unchanged` per coordinator).
    pub fn stop_workspace_auto_admissions(
        &self,
        request: DrainAdmissionsStopRequest<'_>,
    ) -> Result<DrainAdmissionsStopResult, OrbitError> {
        self.require_workspace_claim(STOP_OPERATION, request.claim_token)?;
        let request_id = audit_execution_id("admissions_stop");
        let drain_job_id = workflow_job_id(AUTO_WORKFLOW_ALIAS)?;
        let mut coordinators = self
            .stores()
            .jobs()
            .list_pending_or_running_job_runs(drain_job_id)?;
        // [ORB-13625] A replica's pull drain is this workspace's coordinator
        // too; stopping it closes its window while its leaves keep running
        // and it keeps settling them with the owner. [ORB-13663] The stop
        // then runs a settle-only pass of its own, so settlements a cancelled
        // drain left behind reach the owner without starting another drain.
        coordinators.extend(
            self.stores().jobs().list_pending_or_running_job_runs(
                crate::application::distributed::PULL_DRAIN_JOB,
            )?,
        );

        self.record_stop_request(&request_id, request, drain_job_id, coordinators.len())?;

        if coordinators.is_empty() {
            self.record_stop_completion(
                None,
                &request_id,
                "idle",
                json!({ "coordinator_count": 0 }),
                None,
            )?;
            return Ok(DrainAdmissionsStopResult {
                outcome: "idle",
                coordinators: Vec::new(),
                pull_settlements: self.settle_pending_pulls(),
                replica_owner_machine_id: self.coordination_write_owner().map(str::to_owned),
            });
        }

        let mut changes = Vec::with_capacity(coordinators.len());
        for run in coordinators {
            match self.apply_stop_to_coordinator(&run, request) {
                Ok(change) => {
                    self.record_stop_completion(
                        Some(&change.run_id),
                        &request_id,
                        change.outcome,
                        json!({
                            "job_id": change.job_id,
                            "remaining_children": change
                                .remaining_children
                                .iter()
                                .map(|child| json!({
                                    "run_id": child.run_id,
                                    "job_name": child.job_name,
                                    "phase": child.phase,
                                    "child_status": child.child_status,
                                }))
                                .collect::<Vec<_>>(),
                        }),
                        None,
                    )?;
                    changes.push(change);
                }
                Err(error) => {
                    self.record_stop_completion(
                        Some(&run.run_id),
                        &request_id,
                        "rejected",
                        json!({ "job_id": run.job_id }),
                        Some(error.to_string()),
                    )?;
                    return Err(error);
                }
            }
        }

        let outcome = if changes
            .iter()
            .any(|change| change.outcome == "force_cancelled")
        {
            "force_cancelled"
        } else if changes.iter().any(|change| change.outcome == "stopped") {
            "stopped"
        } else if changes
            .iter()
            .any(|change| change.outcome == "cancelled_queued")
        {
            "cancelled_queued"
        } else {
            "unchanged"
        };
        Ok(DrainAdmissionsStopResult {
            outcome,
            coordinators: changes,
            pull_settlements: self.settle_pending_pulls(),
            replica_owner_machine_id: self.coordination_write_owner().map(str::to_owned),
        })
    }

    /// Whether this run's persisted control forbids further auto admissions.
    pub(crate) fn drain_admissions_stopped(&self, run_id: &str) -> bool {
        self.read_run_state(run_id)
            .ok()
            .flatten()
            .is_some_and(|state| state.admissions_stopped())
    }

    fn apply_stop_to_coordinator(
        &self,
        run: &JobRun,
        request: DrainAdmissionsStopRequest<'_>,
    ) -> Result<DrainAdmissionsStopChange, OrbitError> {
        if run.state == JobRunState::Pending && self.read_run_state(&run.run_id)?.is_none() {
            self.cancel_job_run_with_context(&run.run_id, request.actor, request.source)?;
            return Ok(DrainAdmissionsStopChange {
                run_id: run.run_id.clone(),
                job_id: run.job_id.clone(),
                outcome: "cancelled_queued",
                remaining_children: Vec::new(),
                forced_runs: Vec::new(),
                unstopped_leaves: Vec::new(),
            });
        }
        if run.state.is_terminal() {
            return Err(OrbitError::JobValidation(format!(
                "job run '{}' is {}; a terminal run admits no further work",
                run.run_id, run.state
            )));
        }

        let mut unchanged = false;
        let update = self.stores().jobs().update_run_state(
            &run.run_id,
            &mut |run_state: JobRunState, state: &mut PipelineState| {
                if run_state.is_terminal() {
                    return Err(OrbitError::JobValidation(format!(
                        "job run '{}' is {run_state}; a terminal run admits no further work",
                        run.run_id
                    )));
                }
                if state.admissions_stopped() {
                    unchanged = true;
                    return Ok(());
                }
                state.set_drain_admissions_stop(
                    request.actor.to_string(),
                    request.reason.map(str::to_string),
                );
                Ok(())
            },
        )?;

        match update {
            RunStateUpdate::Updated => {}
            RunStateUpdate::NotFound => {
                return Err(orbit_common::OrbitError::not_found(
                    orbit_common::NotFoundKind::JobRun,
                    run.run_id.clone(),
                ));
            }
            RunStateUpdate::NoState => {
                let mut state = PipelineState::new(
                    run.run_id.clone(),
                    run.job_id.clone(),
                    run.input.clone().unwrap_or_else(|| json!({})),
                );
                state.set_drain_admissions_stop(
                    request.actor.to_string(),
                    request.reason.map(str::to_string),
                );
                self.stores().jobs().write_run_state(&run.run_id, &state)?;
            }
        }

        let mut change = DrainAdmissionsStopChange {
            run_id: run.run_id.clone(),
            job_id: run.job_id.clone(),
            outcome: if unchanged { "unchanged" } else { "stopped" },
            remaining_children: Vec::new(),
            forced_runs: Vec::new(),
            unstopped_leaves: Vec::new(),
        };
        if request.force {
            // Admissions are already stopped, so nothing new starts while the
            // drain and its in-flight work are cancelled.
            let forced = self.cancel_job_run_with_options(
                &run.run_id,
                request.actor,
                request.source,
                request.reason,
                true,
            )?;
            change.outcome = "force_cancelled";
            change.forced_runs = forced.forced_runs;
            change.unstopped_leaves = forced.unstopped_leaves;
        }
        change.remaining_children = self.remaining_children(&run.run_id)?;
        Ok(change)
    }

    /// A local drain's open child dispatches, or the claimed leaves a pull
    /// drain is still carrying (it dispatches none: its leaves are recorded
    /// as pull admissions).
    fn remaining_children(&self, run_id: &str) -> Result<Vec<RemainingDrainChild>, OrbitError> {
        let mut remaining = self
            .pull_drain_claimed_leaves(run_id)?
            .into_iter()
            .map(|leaf| RemainingDrainChild {
                run_id: leaf.leaf_run_id,
                job_name: leaf.job_id,
                phase: format!(
                    "claimed:{}:{}",
                    leaf.task_id.as_deref().unwrap_or("-"),
                    leaf.settlement_phase
                ),
                child_status: Some(leaf.leaf_state),
            })
            .collect::<Vec<_>>();
        let Some(state) = self.read_run_state(run_id)? else {
            return Ok(remaining);
        };
        for dispatch in state.open_child_dispatches() {
            let Some(child) = self.get_job_run_backend(&dispatch.child_run_id)? else {
                continue;
            };
            if child.state.is_terminal() {
                continue;
            }
            remaining.push(RemainingDrainChild {
                run_id: dispatch.child_run_id.clone(),
                job_name: dispatch.job_name.clone(),
                phase: dispatch.phase.as_str().to_string(),
                child_status: Some(child.state.to_string()),
            });
        }
        Ok(remaining)
    }

    fn record_stop_request(
        &self,
        request_id: &str,
        request: DrainAdmissionsStopRequest<'_>,
        drain_job_id: &str,
        coordinator_count: usize,
    ) -> Result<(), OrbitError> {
        self.record_pipeline_audit(
            STOP_REQUEST_AUDIT,
            None,
            Some(request.actor),
            AuditEventStatus::Success,
            json!({
                "request_id": request_id,
                "job_id": drain_job_id,
                "coordinator_count": coordinator_count,
                "reason": request.reason,
                "actor": request.actor,
                "source": request.source,
                "requested_at": chrono::Utc::now().to_rfc3339(),
            }),
            None,
        )
    }

    fn record_stop_completion(
        &self,
        run_id: Option<&str>,
        request_id: &str,
        outcome: &str,
        detail: Value,
        error: Option<String>,
    ) -> Result<(), OrbitError> {
        let mut arguments = json!({
            "request_id": request_id,
            "run_id": run_id,
            "outcome": outcome,
            "completed_at": chrono::Utc::now().to_rfc3339(),
        });
        if let (Some(target), Some(detail)) = (arguments.as_object_mut(), detail.as_object()) {
            for (key, value) in detail {
                target.insert(key.clone(), value.clone());
            }
        }
        self.record_pipeline_audit(
            STOP_COMPLETION_AUDIT,
            run_id,
            None,
            if outcome == "rejected" {
                AuditEventStatus::Failure
            } else {
                AuditEventStatus::Success
            },
            arguments,
            error,
        )
    }
}

fn workflow_job_id(alias: &str) -> Result<&'static str, OrbitError> {
    find_workflow(alias)
        .map(|workflow| workflow.job_id)
        .ok_or_else(|| OrbitError::InvalidInput(format!("unknown workflow '{alias}'")))
}
