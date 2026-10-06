//! Host-qualified execution evidence for desktop completion.
//!
//! A linked run lives in the job store of the machine that executed it
//! (`job_run_machine`). This machine observes its own runs directly. A run
//! another machine executed is known here only through the owner-held claim
//! journal: the claim bound to that exact host and run, and the handoff the
//! owner accepted from it. A run with the same id in this machine's own store
//! is a different run and proves nothing about it.
use orbit_common::OrbitError;
use orbit_engine::RuntimeHost;
use orbit_store::contracts::{ClaimInspection, ClaimRun, ExecutionClaimPhase};
use orbit_types::{
    task::Task,
    workflow::handoff::{AcceptedHandoff, HandoffDelivery},
};
use serde_json::Value;

use crate::OrbitRuntime;

use super::validation::invalid;

/// A linked run another machine executed, with the owner's evidence for it.
pub(crate) struct ForeignExecution {
    run_id: String,
    /// The executing machine as an operator knows it.
    host: String,
    /// The claim bound to exactly this host and run, when this owner holds one.
    claim: Option<ClaimInspection>,
    /// The handoff this owner accepted from that run.
    pub(crate) handoff: Option<AcceptedHandoff>,
}

impl ForeignExecution {
    /// The accepted handoff when it delivered through a pull request.
    pub(crate) fn pull_request_handoff(&self) -> Option<(u64, &AcceptedHandoff)> {
        let accepted = self.handoff.as_ref()?;
        match accepted.handoff.candidate.delivery {
            HandoffDelivery::PullRequest { number } => Some((number, accepted)),
            HandoffDelivery::LocalCandidate
            | HandoffDelivery::AlreadyLanded { .. }
            | HandoffDelivery::NoDiff { .. } => None,
        }
    }

    /// Whether the owner-held evidence proves the execution stopped. Only a
    /// settled claim does: a handoff the owner accepted from the run, a failure
    /// the run settled itself, or a verified landing. An open claim names the
    /// supported way forward instead of a status change it would refuse anyway.
    pub(crate) fn ensure_stopped(&self) -> Result<(), OrbitError> {
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
    pub(crate) fn desktop_foreign_execution(
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

/// A handed-off pull request as the provider reports it now, checked against
/// the handoff's repository, number and landing branch.
pub(crate) struct HandoffPullRequest {
    pub(crate) number: u64,
    pub(crate) url: String,
    pub(crate) state: String,
    pub(crate) head: String,
    pub(crate) merge_commit: Option<String>,
}

impl HandoffPullRequest {
    pub(crate) fn merged(&self) -> bool {
        self.state.eq_ignore_ascii_case("MERGED")
    }
}

impl OrbitRuntime {
    /// One pull request read from the provider by its exact identity, rather
    /// than found in a bounded recent-PR listing. A failed lookup names its
    /// cause.
    pub(crate) fn read_pull_request_status(&self, selector: &str) -> Result<Value, OrbitError> {
        let response = self
            .run_private_vcs_operation(
                "pr.status",
                serde_json::json!({
                    "pr": selector,
                    "workspace_path": self.paths().repo_root.to_string_lossy(),
                }),
            )
            .map_err(|error| {
                invalid(&format!(
                    "pull request {selector} could not be read from the provider: {error}"
                ))
            })?;
        Ok(response["pull_request"].clone())
    }

    /// The accepted handoff's pull request, read by number. The provider's
    /// answer must identify that delivery: its number, repository and landing
    /// branch.
    pub(crate) fn observe_handoff_pull_request(
        &self,
        number: u64,
        accepted: &AcceptedHandoff,
    ) -> Result<HandoffPullRequest, OrbitError> {
        let status = self.read_pull_request_status(&number.to_string())?;
        handoff_pull_request(number, accepted, &status)
    }
}

pub(super) fn handoff_pull_request(
    number: u64,
    accepted: &AcceptedHandoff,
    status: &Value,
) -> Result<HandoffPullRequest, OrbitError> {
    let candidate = &accepted.handoff.candidate;
    let url = reported(status, "url");
    let repository = url.as_deref().and_then(pull_repository);
    let (Some(url), true) = (
        url.clone(),
        status["number"].as_u64() == Some(number)
            && url.as_deref().and_then(pull_number) == Some(number)
            && repository.is_some_and(|slug| slug.eq_ignore_ascii_case(&candidate.repository)),
    ) else {
        return Err(invalid(&format!(
            "the provider's answer for pull request #{number} does not identify the handed-off \
             delivery in {}",
            candidate.repository
        )));
    };
    if reported(status, "baseRefName").as_deref() != Some(candidate.landing_branch.as_str()) {
        return Err(invalid(&format!(
            "pull request #{number} no longer targets the handoff's landing branch {}",
            candidate.landing_branch
        )));
    }
    let head = reported(status, "headRefOid")
        .ok_or_else(|| invalid(&format!("the provider reported no head for #{number}")))?;
    Ok(HandoffPullRequest {
        number,
        url,
        state: reported(status, "state").unwrap_or_default(),
        head,
        merge_commit: reported(&status["mergeCommit"], "oid"),
    })
}

pub(super) fn reported(status: &Value, field: &str) -> Option<String> {
    status[field]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// `https://github.com/<owner>/<repo>/pull/<n>` → `n`.
pub(super) fn pull_number(url: &str) -> Option<u64> {
    let (_, number) = url.trim_end_matches('/').rsplit_once("/pull/")?;
    number.parse().ok()
}

/// `https://github.com/<owner>/<repo>/pull/<n>` → `<owner>/<repo>`.
fn pull_repository(url: &str) -> Option<&str> {
    let (prefix, _) = url.rsplit_once("/pull/")?;
    let path = prefix.strip_prefix("https://github.com/")?;
    (path.split('/').count() == 2).then_some(path)
}
