//! Resident orchestrator mandates: the mandate record, its authority and the
//! reads over both.
//!
//! A mandate is an assignment task tagged `resident` with a
//! `resident/mandate.json` artifact. Its authority lives in the task's
//! history as `resident_authority_changed` entries
//! ([`orbit_types::workflow::ResidentAuthority`]), written only here:
//! [`OrbitRuntime::grant_resident_authority`] applies an operator's widening,
//! turns anyone else's into a `resident_authority` decision request, and
//! applies any task writer's narrowing; the decision answer path applies a
//! human's `grant` through `OrbitRuntime::apply_resident_grant_answer`.
//! Firing, cycles and tool scoping come in later slices.

mod grant;

use orbit_common::OrbitError;
use orbit_types::task::{
    DecisionGate, DecisionRequest, DecisionRequestState, Task, TaskHistoryEntry, TaskStatus,
};
use orbit_types::workflow::{
    RESIDENT_MANDATE_ARTIFACT, RESIDENT_TAG, ResidentAuthority, ResidentMandate, ResidentRight,
};
use serde::Serialize;
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::task::decision_request_json;

pub use grant::{ResidentGrantCaller, ResidentGrantOutcome, ResidentGrantParams};

/// Where a mandate stands, as `orbit resident show` reports it.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResidentMandateState {
    /// Tagged, with a valid mandate, and its assignment still open.
    Active,
    /// The assignment is in review: the resident handed off.
    HandedOff,
    /// The assignment is done, archived or rejected.
    Ended,
    /// Not tagged `resident`, or the mandate artifact is missing or invalid:
    /// the mandate is not due.
    Invalid,
}

/// Whether a task status ends its mandate.
pub(crate) fn resident_mandate_ended(status: TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Done | TaskStatus::Archived | TaskStatus::Rejected
    )
}

/// The rights and workspaces a cycle could act on now: the authority's
/// rights, scoped to workspaces both the authority and the mandate cover.
/// Empty (observe-only) when either record is unavailable.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct ResidentEffectiveAuthority {
    /// Rights held, in contract order.
    pub rights: Vec<ResidentRight>,
    /// Workspaces in scope.
    pub workspaces: Vec<String>,
}

/// One mandate as the reads assemble it.
pub(crate) struct ResidentView {
    pub(crate) task: Task,
    pub(crate) mandate: Result<ResidentMandate, String>,
    pub(crate) authority: Result<ResidentAuthority, String>,
    pub(crate) open_requests: Vec<DecisionRequest>,
}

impl ResidentView {
    pub(crate) fn state(&self) -> (ResidentMandateState, Option<String>) {
        if resident_mandate_ended(self.task.status) {
            return (ResidentMandateState::Ended, None);
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
        (ResidentMandateState::Active, None)
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
    let mandate = match ResidentMandate::parse(&artifact.content) {
        Ok(mandate) => mandate,
        Err(error) => return Ok(Err(error.to_string())),
    };
    if runtime
        .canonical_crew_name(Some(&mandate.crew))
        .ok()
        .flatten()
        .is_none()
    {
        return Ok(Err(format!(
            "mandate crew `{}` is not a configured crew",
            mandate.crew
        )));
    }
    Ok(Ok(mandate))
}

/// The open grant requests in a task's decision requests.
fn open_grant_requests(requests: Vec<DecisionRequest>) -> Vec<DecisionRequest> {
    requests
        .into_iter()
        .filter(|request| {
            request.gate == DecisionGate::ResidentAuthority
                && request.state == DecisionRequestState::Open
        })
        .collect()
}

fn to_json<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

impl OrbitRuntime {
    pub(crate) fn resident_view(
        &self,
        task: Task,
        history: &[TaskHistoryEntry],
    ) -> Result<ResidentView, OrbitError> {
        let mandate = resident_mandate_from_artifact(self, &task.id)?;
        let authority = ResidentAuthority::from_history(history);
        let open_requests = open_grant_requests(self.get_task_decision_requests(&task.id)?);
        Ok(ResidentView {
            task,
            mandate,
            authority,
            open_requests,
        })
    }

    /// `orbit.resident.show`: the mandate, its effective authority, the
    /// authority record with its grant history, open grant requests and the
    /// mandate's state.
    pub fn show_resident_mandate(&self, task_id: &str) -> Result<Value, OrbitError> {
        let task = self.get_task(task_id.trim())?;
        let history = self.get_task_history(&task.id)?;
        let view = self.resident_view(task, &history)?;
        let (state, reason) = view.state();
        let mut value = json!({
            "task_id": view.task.id,
            "title": view.task.title,
            "status": view.task.status,
            "state": state,
            "mandate": view.mandate.as_ref().ok().map(to_json),
            "effective_authority": to_json(&view.effective()),
            "authority": view.authority.as_ref().ok().map(to_json),
            "open_requests": view
                .open_requests
                .iter()
                .map(decision_request_json)
                .collect::<Vec<_>>(),
        });
        if let Some(reason) = reason {
            value["state_reason"] = json!(reason);
        }
        if let Err(error) = &view.mandate {
            value["mandate_error"] = json!(error);
        }
        if let Err(error) = &view.authority {
            value["authority_error"] = json!(format!("authority_unavailable: {error}"));
        }
        Ok(value)
    }

    /// `orbit.resident.list`: every task tagged `resident`, ended ones only
    /// with `include_ended`.
    pub fn list_resident_mandates(&self, include_ended: bool) -> Result<Value, OrbitError> {
        let mut rows = Vec::new();
        for task in self.list_task_metadata()? {
            if !task.tags.iter().any(|tag| tag == RESIDENT_TAG) {
                continue;
            }
            if !include_ended && resident_mandate_ended(task.status) {
                continue;
            }
            let history = self.get_task_history(&task.id)?;
            let view = self.resident_view(task, &history)?;
            let (state, reason) = view.state();
            let effective = view.effective();
            let mandate = view.mandate.as_ref().ok();
            rows.push(json!({
                "task_id": view.task.id,
                "title": view.task.title,
                "status": view.task.status,
                "state": state,
                "state_reason": reason,
                "goal": mandate.map(|mandate| mandate.goal.clone()),
                "deadline": mandate.and_then(|mandate| mandate.deadline),
                "rights": effective.rights,
                "workspaces": effective.workspaces,
                "authority_revision": view.authority.as_ref().ok().map(|authority| authority.revision),
                "open_requests": view.open_requests.len(),
            }));
        }
        Ok(json!({ "count": rows.len(), "mandates": rows }))
    }
}
