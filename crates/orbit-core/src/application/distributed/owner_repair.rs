//! The owner's fallback for a stopped landing's repair [ORB-17303].
//!
//! A landing that stops on a repairable failure leaves its claim
//! `repair_pending`, reserved for the executor that delivered it. When that
//! executor stops pulling — its admissions stopped, the host offline, its
//! window over — the owner's own `orbit run auto` drain takes the repair once
//! the store's fallback window has passed. The owner wave admits backlog work
//! only, so the drain does it through the same path a follower would: a
//! one-pass, repair-only pull drain against this owner, whose request the
//! owner admits under its local identity with the same crew, OS, browser-kit
//! and capacity screens, and whose admission revokes the superseded claim.
//!
//! The marker on that pull's run input is what makes the owner's admission
//! repair-only; it is read back from this host's own job store, never from
//! the request, so nothing on the wire changes.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::{
    AdmissionIdentity, AdmissionRequest, JobRunQuery, PendingRepair, PullDestination,
};
use orbit_types::workflow::{DrainWaitingTask, JobRun};
use serde_json::{Value, json};

use super::PULL_DRAIN_JOB;
use crate::OrbitRuntime;
use crate::application::job::pipeline::{ChildPipelineAdmission, ChildSubmission};
use crate::runtime::engine::crew::ALLOWED_CREWS_INPUT_KEY;

/// The pull-drain run-input key marking the owner's own repair pull.
const OWNER_REPAIR_PULL_INPUT_KEY: &str = "owner_repair_pull";

/// `drain_last_pass` reason for a repair still reserved for its original
/// executor.
const REPAIR_RESERVED_REASON: &str = "repair_reserved";

/// `drain_last_pass` reason for a repair the owner may take now.
const REPAIR_FALLBACK_REASON: &str = "repair_owner_fallback";

/// How long after a repair pull ended without taking a repair the drain waits
/// before starting another, so a repair this host cannot run does not start a
/// pull every pass.
const REPAIR_PULL_RETRY_SECONDS: i64 = 10 * 60;

/// What the owner drain's pass decided for the pending repairs.
#[derive(Debug, Default)]
pub(crate) struct OwnerRepairPass {
    /// Every pending repair, with why it waits or which pull is taking it.
    pub(crate) waiting: Vec<DrainWaitingTask>,
    /// The repair pull this pass started.
    pub(crate) started: Option<String>,
    /// Slots the started pull may fill, kept from this pass's backlog wave.
    pub(crate) reserved_slots: usize,
}

/// The owner drain pass's view the fallback decides from.
pub(crate) struct OwnerRepairDrain<'a> {
    pub(crate) drain_run_id: Option<&'a str>,
    pub(crate) action: &'a str,
    /// Why the drain admits nothing at all this pass, if it does not.
    pub(crate) held: Option<&'a str>,
    pub(crate) free_slots: usize,
}

impl OrbitRuntime {
    /// Whether `request` is this owner's own repair pull: a local caller
    /// whose run is a pull drain this host's drain started for a repair.
    pub(crate) fn is_owner_repair_pull(
        &self,
        identity: &AdmissionIdentity,
        request: &AdmissionRequest,
    ) -> Result<bool, OrbitError> {
        if identity.is_remote() {
            return Ok(false);
        }
        Ok(self
            .stores()
            .jobs()
            .get_job_run(&request.run_context.run_id)?
            .is_some_and(|run| is_repair_pull(&run)))
    }

    /// Report each pending repair on the owner drain's pass, and start a
    /// repair-only pull for those whose fallback window has passed.
    pub(crate) fn owner_repair_fallback(
        &self,
        drain: &OwnerRepairDrain<'_>,
    ) -> Result<OwnerRepairPass, OrbitError> {
        // A replica's checkout serves no claims.
        if self.coordination_write_owner().is_some() {
            return Ok(OwnerRepairPass::default());
        }
        let Some(location) = self.automation_execution_location().cloned() else {
            return Ok(OwnerRepairPass::default());
        };
        let owner = AdmissionIdentity::trusted_local(location.clone());
        let repairs = self.stores().tasks().pending_repairs(&owner)?;
        if repairs.is_empty() {
            return Ok(OwnerRepairPass::default());
        }
        let mut pass = OwnerRepairPass::default();
        let (reserved, due): (Vec<PendingRepair>, Vec<PendingRepair>) = repairs
            .into_iter()
            .partition(|repair| repair.wait.is_some());
        pass.waiting
            .extend(reserved.into_iter().map(|repair| DrainWaitingTask {
                task_id: repair.task_id,
                reason: Some(REPAIR_RESERVED_REASON.into()),
                blocked_by: Vec::new(),
                detail: repair.wait,
                findings: Vec::new(),
            }));
        if due.is_empty() {
            return Ok(pass);
        }
        let detail = match self.repair_pull_wait(drain)? {
            Some(wait) => wait,
            None => match self.submit_owner_repair_pull(drain, &location.machine_id, due.len()) {
                Ok(Some(run_id)) => {
                    pass.reserved_slots = due.len().min(drain.free_slots);
                    let detail = format!(
                        "its original executor has not taken it within the fallback window; \
                         the owner's repair pull {run_id} is taking it"
                    );
                    pass.started = Some(run_id);
                    detail
                }
                Ok(None) => "its original executor has not taken it within the fallback \
                             window; the owner's repair pull waits for this drain's admissions \
                             to reopen"
                    .into(),
                // Not a decision: the next pass tries again.
                Err(error) => {
                    tracing::warn!(
                        target: "orbit.core.drain",
                        error = %error,
                        "the owner drain could not start a repair pull"
                    );
                    format!(
                        "its original executor has not taken it within the fallback window; \
                         the owner's repair pull could not start ({error}) and is retried next \
                         pass"
                    )
                }
            },
        };
        pass.waiting
            .extend(due.into_iter().map(|repair| DrainWaitingTask {
                task_id: repair.task_id,
                reason: Some(REPAIR_FALLBACK_REASON.into()),
                blocked_by: Vec::new(),
                detail: Some(detail.clone()),
                findings: Vec::new(),
            }));
        Ok(pass)
    }

    /// Why no repair pull may start this pass, or `None` when one may.
    fn repair_pull_wait(&self, drain: &OwnerRepairDrain<'_>) -> Result<Option<String>, OrbitError> {
        const DUE: &str = "its original executor has not taken it within the fallback window";
        if let Some(held) = drain.held {
            return Ok(Some(format!(
                "{DUE}; the owner's repair pull waits while this drain admits nothing ({held})"
            )));
        }
        if drain.drain_run_id.is_none() {
            return Ok(Some(format!(
                "{DUE}; only a running `orbit run auto` drain starts the owner's repair pull"
            )));
        }
        if self.drain_owner_transport().is_none() {
            return Ok(Some(format!(
                "{DUE}; this runtime has no route to its own owner tools, so it cannot pull the \
                 repair"
            )));
        }
        let jobs = self.stores().jobs();
        if let Some(live) = jobs
            .list_pending_or_running_job_runs(PULL_DRAIN_JOB)?
            .into_iter()
            .find(is_repair_pull)
        {
            return Ok(Some(format!(
                "{DUE}; the owner's repair pull {} is taking it",
                live.run_id
            )));
        }
        if drain.free_slots == 0 {
            return Ok(Some(format!(
                "{DUE}; the owner's repair pull waits for a free slot"
            )));
        }
        let last = jobs
            .list_job_runs_filtered(&JobRunQuery {
                job_id: Some(PULL_DRAIN_JOB.to_string()),
                terminal_only: true,
                include_steps: false,
                ..JobRunQuery::default()
            })?
            .into_iter()
            .filter(is_repair_pull)
            .filter_map(|run| run.finished_at.map(|finished| (finished, run.run_id)))
            .max();
        if let Some((finished, run_id)) = last
            && let Some(retry_at) = retry_after(finished)
            && Utc::now() < retry_at
        {
            return Ok(Some(format!(
                "{DUE}; the owner's last repair pull {run_id} did not take it (its last pass says \
                 why), and the next starts at {}",
                retry_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
            )));
        }
        Ok(None)
    }

    /// Submit a one-pass, repair-only pull against this owner as a child of
    /// the drain, with the drain's crew restriction. `None` when the drain
    /// acknowledged an admissions stop first.
    fn submit_owner_repair_pull(
        &self,
        drain: &OwnerRepairDrain<'_>,
        machine_id: &str,
        repairs: usize,
    ) -> Result<Option<String>, OrbitError> {
        let Some(parent) = drain.drain_run_id else {
            return Ok(None);
        };
        let logical = match self.workspace_runtime_binding() {
            Some(binding) => binding.logical_workspace_id.clone(),
            None => self.workspace_id()?,
        };
        let destination = PullDestination {
            owner_machine_id: machine_id.to_string(),
            selector: format!("{machine_id}/{logical}"),
            owner_workspace_id: logical,
            execution_machine_id: machine_id.to_string(),
        };
        let mut input = json!({
            "for_seconds": 0,
            "destination": serde_json::to_value(&destination)
                .map_err(|error| OrbitError::Store(error.to_string()))?,
            "owner_validation_gates": self.owner_validation_gates(),
            "max_active_leaf_runs": repairs.min(drain.free_slots).max(1),
            OWNER_REPAIR_PULL_INPUT_KEY: true,
        });
        if let Some(crews) = self
            .stores()
            .jobs()
            .get_job_run(parent)?
            .and_then(|run| run.input)
            .and_then(|input| input.get(ALLOWED_CREWS_INPUT_KEY).cloned())
            .filter(|crews| !crews.is_null())
        {
            input[ALLOWED_CREWS_INPUT_KEY] = crews;
        }
        let submitted = self.submit_child_pipeline_run(
            PULL_DRAIN_JOB,
            input,
            None,
            Some("orbit-auto-drain"),
            &ChildPipelineAdmission {
                parent_run_id: parent.to_string(),
                parent_step_id: None,
                action: drain.action.to_string(),
                blocking: false,
            },
        )?;
        Ok(match submitted {
            ChildSubmission::Submitted(result) | ChildSubmission::Resolved(result) => {
                Some(result.run_id)
            }
            ChildSubmission::Skipped(_) => None,
        })
    }
}

fn is_repair_pull(run: &JobRun) -> bool {
    run.job_id == PULL_DRAIN_JOB
        && run
            .input
            .as_ref()
            .and_then(|input| input.get(OWNER_REPAIR_PULL_INPUT_KEY))
            .and_then(Value::as_bool)
            == Some(true)
}

fn retry_after(finished: DateTime<Utc>) -> Option<DateTime<Utc>> {
    finished.checked_add_signed(chrono::Duration::seconds(REPAIR_PULL_RETRY_SECONDS))
}
