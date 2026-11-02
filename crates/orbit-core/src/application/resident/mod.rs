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
//! A cycle fires through the `resident_due` state trigger (`due`), runs
//! as `resident_cycle_pipeline` (`steps`) and reaches tools only through
//! the cycle guard (`guard`). The manager pauses, stops, wakes or takes a
//! mandate over and relays the human's words to it through `control`.

mod comments;
mod control;
mod cycle;
mod digest;
mod due;
mod grant;
mod guard;
mod metrics;
mod steps;

#[cfg(feature = "test-support")]
pub use cycle::test_support::with_resident_cycle_admission_checkpoint;

use orbit_common::OrbitError;
use orbit_types::task::{
    DecisionGate, DecisionRequest, DecisionRequestState, Task, TaskHistoryEntry, TaskStatus,
};
use orbit_types::workflow::{
    RESIDENT_MANDATE_ARTIFACT, RESIDENT_TAG, ResidentAuthority, ResidentComment, ResidentMandate,
    ResidentRight,
};
use serde::Serialize;
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::task::decision_request_json;

pub(crate) use comments::ensure_resident_comment_redaction_safe;
pub use control::{
    ResidentChannelCaller, ResidentControlAction, ResidentControlParams, ResidentControlState,
    ResidentRelayParams,
};
pub(crate) use due::ResidentDue;
pub use grant::{ResidentGrantCaller, ResidentGrantOutcome, ResidentGrantParams};
pub(crate) use guard::{ResidentCallAdmission, ResidentCallClass};
pub use steps::{RESIDENT_CYCLE_LEASE_MINUTES, resident_cycle_id};

/// Where a mandate stands, as `orbit resident show` reports it.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResidentMandateState {
    /// Tagged, with a valid mandate, and its assignment still open.
    Active,
    /// The assignment is in review: the resident handed off.
    HandedOff,
    /// Paused by `orbit resident control pause`: no new cycle.
    Paused,
    /// Taken over by a person: no cycle fires until `release`.
    TakenOver,
    /// The assignment is done, archived or rejected, or the mandate was
    /// stopped.
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

/// The control record as show reports it.
fn control_json(view: &ResidentView) -> Value {
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

impl OrbitRuntime {
    /// Why an active mandate is stalled, as attention and the reads report
    /// it; `None` for any other state.
    pub(crate) fn resident_stall_of(
        &self,
        view: &ResidentView,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<due::ResidentStall>, OrbitError> {
        let Ok(mandate) = &view.mandate else {
            return Ok(None);
        };
        if view.state().0 != ResidentMandateState::Active {
            return Ok(None);
        }
        let comments = self.get_task_comments(&view.task.id)?;
        Ok(due::resident_stall(&view.task, mandate, &comments, now))
    }

    pub(crate) fn resident_view(
        &self,
        task: Task,
        history: &[TaskHistoryEntry],
    ) -> Result<ResidentView, OrbitError> {
        let mandate = resident_mandate_from_artifact(self, &task.id)?;
        let authority = ResidentAuthority::from_history(history);
        let open_requests = open_grant_requests(self.get_task_decision_requests(&task.id)?);
        let control = control::resident_control_from_history(history);
        Ok(ResidentView {
            task,
            mandate,
            authority,
            open_requests,
            control,
        })
    }

    /// `orbit.resident.show`: the mandate, its effective authority, the
    /// authority record with its grant history, open grant requests, live
    /// cycle, latest parsed checkpoint and the mandate's state.
    pub fn show_resident_mandate(&self, task_id: &str) -> Result<Value, OrbitError> {
        let task = self.get_task(task_id.trim())?;
        let history = self.get_task_history(&task.id)?;
        let view = self.resident_view(task, &history)?;
        let latest_checkpoint = self
            .get_task_comments(&view.task.id)?
            .iter()
            .rev()
            .find_map(
                |comment| match ResidentComment::parse(&comment.message).ok()? {
                    record @ ResidentComment::Checkpoint(_)
                        if record.mandate() == view.task.id.as_str() =>
                    {
                        Some(to_json(&record))
                    }
                    _ => None,
                },
            );
        let live_cycle = cycle::live_cycle_from_history(&history)?
            .filter(|live| live.expires_at > chrono::Utc::now());
        let (state, reason) = view.state();
        let stall = self.resident_stall_of(&view, chrono::Utc::now())?;
        let mut value = json!({
            "task_id": view.task.id,
            "control": control_json(&view),
            "stalled": stall.is_some(),
            "stalled_reason": stall.map(|stall| stall.reason),
            "title": view.task.title,
            "status": view.task.status,
            "latest_checkpoint": latest_checkpoint,
            "live_cycle": live_cycle,
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
            let stalled = self.resident_stall_of(&view, chrono::Utc::now())?.is_some();
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
                "stalled": stalled,
            }));
        }
        Ok(json!({ "count": rows.len(), "mandates": rows }))
    }
}
