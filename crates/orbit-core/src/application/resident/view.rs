//! Resident mandate view, artifact reads and JSON projections.

use orbit_common::OrbitError;
use orbit_types::task::{DecisionGate, DecisionRequest, DecisionRequestState, Task, TaskStatus};
use orbit_types::workflow::{
    RESIDENT_MANDATE_ARTIFACT, RESIDENT_TAG, ResidentAuthority, ResidentMandate,
};
use serde::Serialize;
use serde_json::{Value, json};

use super::{
    ResidentControlState, ResidentEffectiveAuthority, ResidentMandateState, control,
    resident_mandate_ended,
};
use crate::OrbitRuntime;

/// One mandate as the reads assemble it.
pub(crate) struct ResidentView {
    pub(crate) task: Task,
    pub(crate) mandate: Result<ResidentMandate, String>,
    pub(crate) authority: Result<ResidentAuthority, String>,
    pub(crate) open_requests: Vec<DecisionRequest>,
    pub(crate) control: Result<control::ResidentControl, String>,
}

impl ResidentView {
    pub(crate) fn state(&self) -> (ResidentMandateState, Option<String>) {
        if resident_mandate_ended(self.task.status) {
            return (ResidentMandateState::Ended, None);
        }
        let control = match &self.control {
            Ok(control) => control,
            Err(reason) => return (ResidentMandateState::Invalid, Some(reason.clone())),
        };
        let control_reason = || {
            control.last.as_ref().map(|(entry, _)| {
                format!(
                    "{} by {}: {}",
                    entry.action.as_str(),
                    entry.by,
                    entry.reason
                )
            })
        };
        if control.state == ResidentControlState::Ended {
            return (ResidentMandateState::Ended, control_reason());
        }
        if !self.task.tags.iter().any(|tag| tag == RESIDENT_TAG) {
            return (
                ResidentMandateState::Invalid,
                Some(format!("the task is not tagged `{RESIDENT_TAG}`")),
            );
        }
        if let Err(reason) = &self.mandate {
            return (ResidentMandateState::Invalid, Some(reason.clone()));
        }
        if self.task.status == TaskStatus::Review {
            return (ResidentMandateState::HandedOff, None);
        }
        match control.state {
            ResidentControlState::Paused => (ResidentMandateState::Paused, control_reason()),
            ResidentControlState::TakenOver => (ResidentMandateState::TakenOver, control_reason()),
            ResidentControlState::Active | ResidentControlState::Ended => {
                (ResidentMandateState::Active, None)
            }
        }
    }

    pub(crate) fn effective(&self) -> ResidentEffectiveAuthority {
        let (Ok(mandate), Ok(authority)) = (&self.mandate, &self.authority) else {
            return ResidentEffectiveAuthority::default();
        };
        if matches!(
            self.state().0,
            ResidentMandateState::Ended | ResidentMandateState::Invalid
        ) {
            return ResidentEffectiveAuthority::default();
        }
        let workspaces: Vec<String> = authority
            .workspaces
            .iter()
            .filter(|workspace| mandate.covers_workspace(workspace))
            .cloned()
            .collect();
        ResidentEffectiveAuthority {
            rights: authority.rights.clone(),
            workspaces,
        }
    }
}

/// Read a task's mandate artifact, or why it is not usable.
pub(crate) fn resident_mandate_from_artifact(
    runtime: &OrbitRuntime,
    task_id: &str,
) -> Result<Result<ResidentMandate, String>, OrbitError> {
    let Some(artifact) = runtime.get_task_artifact(task_id, RESIDENT_MANDATE_ARTIFACT)? else {
        return Ok(Err(format!(
            "the task has no {RESIDENT_MANDATE_ARTIFACT} artifact"
        )));
    };
    Ok(runtime.check_resident_mandate(&artifact.content))
}

/// The open grant requests in a task's decision requests.
pub(super) fn open_grant_requests(requests: Vec<DecisionRequest>) -> Vec<DecisionRequest> {
    requests
        .into_iter()
        .filter(|request| {
            request.gate == DecisionGate::ResidentAuthority
                && request.state == DecisionRequestState::Open
        })
        .collect()
}

pub(super) fn to_json<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

/// The control record as show reports it.
pub(super) fn control_json(view: &ResidentView) -> Value {
    match &view.control {
        Ok(control) => {
            let mut value = json!({"state": control.state});
            if let Some((entry, at)) = &control.last {
                value["last"] = json!({
                    "action": entry.action,
                    "at": at,
                    "by": entry.by,
                    "reason": entry.reason,
                    "status_request": entry.status_request,
                    "by_session": entry.by_session,
                });
            }
            value
        }
        Err(error) => json!({"error": error}),
    }
}
