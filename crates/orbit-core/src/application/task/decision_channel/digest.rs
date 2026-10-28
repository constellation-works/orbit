use orbit_common::{DecisionRefusalCode, OrbitError};
use orbit_types::task::{
    DECISION_DIGEST_EXPIRY_WINDOW_HOURS, DecisionBinding, DecisionGate, DecisionRequest,
    DecisionRequestState, TaskStatus, current_candidate_identity,
};

use crate::OrbitRuntime;
use crate::application::task::helpers::SYSTEM_ACTOR_LABEL;
use crate::application::task::{TaskRecordUpdateParams, TaskRecords};

use super::model::{DecisionDigest, DecisionSubmitCaller};

impl OrbitRuntime {
    /// The decision digest one transport call renders (design §7):
    /// raise the approval request every `proposed` task is missing (and
    /// every `review` task's, bound to its candidate), then return those
    /// requests beside every still-open request expiring within
    /// [`DECISION_DIGEST_EXPIRY_WINDOW_HOURS`].
    ///
    /// Raising is idempotent per (task, gate, binding): a task whose
    /// current binding already has an open request raises nothing and
    /// the digest returns that request, so a second call returns the
    /// same request ids. The caller is the channel's bound plugin
    /// through its callback session, or the operator — an agent session
    /// is refused `caller_not_allowed`, and a plugin the named channel
    /// is not bound to is refused `channel_not_registered`, exactly as
    /// for submission. With `[decision_channel]` disabled the digest
    /// raises nothing, writes nothing and returns no requests.
    pub fn decision_digest(
        &self,
        channel: &str,
        caller: DecisionSubmitCaller,
    ) -> Result<DecisionDigest, OrbitError> {
        match &caller {
            DecisionSubmitCaller::PluginCallback { .. } | DecisionSubmitCaller::Operator => {}
            DecisionSubmitCaller::Agent | DecisionSubmitCaller::Unknown => {
                return Err(OrbitError::decision_refused(
                    DecisionRefusalCode::CallerNotAllowed,
                    "the decision digest is a registered channel's or the operator's; an agent session never raises requests for a human",
                ));
            }
        }
        if !self.decision_channel_enabled() {
            return Ok(DecisionDigest {
                requests: Vec::new(),
                raised: Vec::new(),
                expiring: Vec::new(),
                warnings: Vec::new(),
            });
        }
        let registration = self.active_registration(channel)?;
        if let DecisionSubmitCaller::PluginCallback { plugin } = &caller
            && plugin != &registration.plugin
        {
            return Err(OrbitError::decision_refused(
                DecisionRefusalCode::ChannelNotRegistered,
                format!(
                    "decision channel `{channel}` is not bound to plugin `{plugin}`; nothing was raised"
                ),
            ));
        }
        self.ensure_coordination_task_write_permitted()?;
        let mut raised = Vec::new();
        let mut approvals = Vec::new();
        let mut warnings = Vec::new();
        for (status, gate) in [
            (TaskStatus::Proposed, DecisionGate::ProposedApproval),
            (TaskStatus::Review, DecisionGate::ReviewApproval),
        ] {
            for task in self.list_tasks_filtered(Some(status), None, None, None, None, None)? {
                if let Some(Some((request, fresh))) =
                    crate::application::task::listing::task_bundle_scan_result(
                        self.raise_approval_decision_request(&task.id, gate),
                        &mut warnings,
                    )?
                {
                    if fresh {
                        raised.push(request.clone());
                    }
                    approvals.push(request);
                }
            }
        }
        let horizon =
            chrono::Utc::now() + chrono::Duration::hours(DECISION_DIGEST_EXPIRY_WINDOW_HOURS);
        let listing = self.list_decision_requests_with_warnings(None, None, false)?;
        for warning in listing.warnings {
            if !warnings.contains(&warning) {
                warnings.push(warning);
            }
        }
        let expiring: Vec<DecisionRequest> = listing
            .requests
            .into_iter()
            .filter(|request| request.expires_at <= horizon)
            .collect();
        let mut requests = approvals;
        for request in &expiring {
            if !requests.iter().any(|carried| carried.id == request.id) {
                requests.push(request.clone());
            }
        }
        requests.sort_by(|a, b| (a.raised_at, &a.id).cmp(&(b.raised_at, &b.id)));
        Ok(DecisionDigest {
            requests,
            raised,
            expiring,
            warnings,
        })
    }

    /// The one open approval request for `task_id`'s current binding on
    /// `gate`, raising it (`raised_by: digest`) when the binding has
    /// none. The second element says whether this call raised it.
    /// `None` when the task left the gate's status; the caller has
    /// already checked the channel is enabled.
    fn raise_approval_decision_request(
        &self,
        task_id: &str,
        gate: DecisionGate,
    ) -> Result<Option<(DecisionRequest, bool)>, OrbitError> {
        let mut outcome: Option<(DecisionRequest, bool)> = None;
        self.stores()
            .tasks()
            .with_task_write_lock(task_id, &mut || {
                let task = self.get_task(task_id)?;
                let expected = match gate {
                    DecisionGate::ProposedApproval => TaskStatus::Proposed,
                    DecisionGate::ReviewApproval => TaskStatus::Review,
                    DecisionGate::BlockTriage | DecisionGate::PilotAdmission => {
                        return Ok(());
                    }
                };
                if task.status != expected {
                    return Ok(());
                }
                let candidate = if gate == DecisionGate::ReviewApproval {
                    current_candidate_identity(&self.get_task_history(task_id)?)
                } else {
                    None
                };
                let binding = DecisionBinding::for_task(&task, candidate);
                let existing =
                    self.get_task_decision_requests(task_id)?
                        .into_iter()
                        .find(|request| {
                            request.gate == gate
                                && request.binding == binding
                                && request.state == DecisionRequestState::Open
                        });
                if let Some(existing) = existing {
                    outcome = Some((existing, false));
                    return Ok(());
                }
                let request = DecisionRequest::new(
                    task_id.to_string(),
                    gate,
                    gate.approval_answers(),
                    binding,
                    "digest".to_string(),
                    chrono::Utc::now(),
                );
                self.stores().task_records().update(
                    task_id,
                    TaskRecordUpdateParams {
                        actor: SYSTEM_ACTOR_LABEL.to_string(),
                        append_history: vec![request.history_entry(SYSTEM_ACTOR_LABEL)],
                        ..Default::default()
                    },
                )?;
                let stored = self
                    .get_task_decision_requests(task_id)?
                    .into_iter()
                    .find(|candidate| candidate.id == request.id)
                    .unwrap_or(request);
                outcome = Some((stored, true));
                Ok(())
            })?;
        Ok(outcome)
    }
}
