//! Host-qualified execution evidence for desktop completion.
//!
//! A linked run lives in the job store of the machine that executed it
//! (`job_run_machine`). This machine observes its own runs directly. A run
//! another machine executed is known here only through the owner-held claim
//! journal: the claim bound to that exact host and run, and the handoff the
//! owner accepted from it. A run with the same id in this machine's own store
//! is a different run and proves nothing about it.
use orbit_common::OrbitError;
use orbit_store::contracts::{ClaimInspection, ClaimRun, ExecutionClaimPhase};
use orbit_types::{task::Task, workflow::handoff::AcceptedHandoff};

use crate::OrbitRuntime;

use super::validation::invalid;

/// A linked run another machine executed, with the owner's evidence for it.
pub(super) struct ForeignExecution {
    run_id: String,
    /// The executing machine as an operator knows it.
    host: String,
    /// The claim bound to exactly this host and run, when this owner holds one.
    claim: Option<ClaimInspection>,
    /// The handoff this owner accepted from that run.
    pub(super) handoff: Option<AcceptedHandoff>,
}

impl ForeignExecution {
    /// Whether the owner-held evidence proves the execution stopped. Only a
    /// settled claim does: a handoff the owner accepted from the run, a failure
    /// the run settled itself, or a verified landing. An open claim names the
    /// supported way forward instead of a status change it would refuse anyway.
    pub(super) fn ensure_stopped(&self) -> Result<(), OrbitError> {
        let (run, host) = (&self.run_id, &self.host);
        let Some(claim) = &self.claim else {
            return Err(invalid(&format!(
                "run {run} executed on {host}, and this owner holds no claim bound to that run; \
                 completion cannot verify that the execution stopped (a run with the same id on \
                 this machine is a different run)"
            )));
        };
        match claim.claim.phase {
            ExecutionClaimPhase::Claimed | ExecutionClaimPhase::Running => Err(invalid(&format!(
                "run {run} on {host} is still executing under its claim; wait for the run to \
                     settle, or recover the claim from the owner's operator console"
            ))),
            ExecutionClaimPhase::HandedOff => Err(invalid(&format!(
                "the handoff from run {run} on {host} still holds its claim and fences this task; \
                 let the owner land it through its completion authority, or revoke the handoff \
                 and recover the claim from the owner's operator console, before completing \
                 from review"
            ))),
            ExecutionClaimPhase::Revoked if self.handoff.is_none() => Err(invalid(&format!(
                "the claim for run {run} on {host} was recovered before the run handed off; no \
                 owner-held evidence shows that the execution stopped, so inspect the run on \
                 {host}"
            ))),
            ExecutionClaimPhase::Revoked
            | ExecutionClaimPhase::Failed
            | ExecutionClaimPhase::Landed => Ok(()),
        }
    }
}

impl OrbitRuntime {
    /// The owner-held evidence for `task`'s linked run when another machine
    /// executed it; `None` when no run is linked or this machine ran it. An
    /// unknown local identity never matches a recorded host.
    pub(super) fn desktop_foreign_execution(
        &self,
        task: &Task,
    ) -> Result<Option<ForeignExecution>, OrbitError> {
        let (Some(run_id), Some(location)) = (&task.job_run_id, &task.job_run_machine) else {
            return Ok(None);
        };
        if self.automation_machine_identity() == Some(location.machine_id.as_str()) {
            return Ok(None);
        }
        let bound = ClaimRun {
            machine_id: location.machine_id.clone(),
            run_id: run_id.clone(),
        };
        let tasks = self.stores().tasks();
        let claim = tasks.resolve_execution_claims()?.into_iter().find(|claim| {
            claim.claim.task_id == task.id && claim.bound_run.as_ref() == Some(&bound)
        });
        let handoff = match &claim {
            Some(claim) => tasks.find_accepted_handoff(&claim.claim.claim_id)?,
            None => None,
        };
        if handoff.as_ref().is_some_and(|accepted| {
            accepted.handoff.run_id != bound.run_id
                || accepted.handoff.machine_id != bound.machine_id
        }) {
            return Err(invalid(
                "the accepted handoff names a different run than the task's linked execution",
            ));
        }
        Ok(Some(ForeignExecution {
            run_id: bound.run_id,
            host: location
                .machine_name
                .clone()
                .unwrap_or_else(|| location.machine_id.clone()),
            claim,
            handoff,
        }))
    }
}
