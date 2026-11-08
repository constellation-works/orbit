//! Runtime reads of resident mandates and their current state.

use orbit_common::OrbitError;
use orbit_types::task::{Task, TaskHistoryEntry};
use orbit_types::workflow::{RESIDENT_TAG, ResidentAuthority, ResidentComment, ResidentMandate};
use serde_json::{Value, json};

use super::{
    ResidentMandateState, ResidentRoutineStatusPort, ResidentView, control, cycle, due,
    resident_cycle_routine_disabled_reason, resident_mandate_ended, resident_mandate_from_artifact,
    view,
};
use crate::OrbitRuntime;
use crate::application::task::decision_request_json;

impl OrbitRuntime {
    /// Judge mandate bytes the way every mandate read does: the
    /// `resident.mandate/v1` field rules and a configured crew. The error is
    /// the message `orbit resident show` reports as `mandate_error`, so a
    /// form that checks a draft here refuses it with the server's words.
    pub fn check_resident_mandate(&self, bytes: &[u8]) -> Result<ResidentMandate, String> {
        let mandate = ResidentMandate::parse(bytes).map_err(|error| error.to_string())?;
        if self
            .canonical_crew_name(Some(&mandate.crew))
            .ok()
            .flatten()
            .is_none()
        {
            return Err(format!(
                "mandate crew `{}` is not a configured crew",
                mandate.crew
            ));
        }
        Ok(mandate)
    }
}

impl OrbitRuntime {
    /// The name of this workspace's resident cycle routine when it is
    /// loaded and its versioned `enabled` is false; `None` when it is
    /// enabled, missing, or its status cannot be read (a failed lookup
    /// never fails the caller's read).
    pub(crate) fn resident_cycle_routine_disabled(&self) -> Option<String> {
        self.disabled_resident_cycle_routine_name()
    }

    /// Why an active mandate is stalled, as attention and the reads report
    /// it; `None` for any other state. When the workspace's cycle routine
    /// is disabled (`cycle_routine_disabled` names it), that is named as
    /// the cause, since no cycle can fire until it is enabled.
    pub(crate) fn resident_stall_of(
        &self,
        view: &ResidentView,
        now: chrono::DateTime<chrono::Utc>,
        cycle_routine_disabled: Option<&str>,
    ) -> Result<Option<due::ResidentStall>, OrbitError> {
        let Ok(mandate) = &view.mandate else {
            return Ok(None);
        };
        if view.state().0 != ResidentMandateState::Active {
            return Ok(None);
        }
        let comments = self.get_task_comments(&view.task.id)?;
        let mut stall = due::resident_stall(&view.task, mandate, &comments, now);
        if let (Some(stall), Some(routine_name)) = (&mut stall, cycle_routine_disabled) {
            stall.reason = format!(
                "{}; {}",
                stall.reason,
                resident_cycle_routine_disabled_reason(routine_name)
            );
        }
        Ok(stall)
    }

    pub(crate) fn resident_view(
        &self,
        task: Task,
        history: &[TaskHistoryEntry],
    ) -> Result<ResidentView, OrbitError> {
        let mandate = resident_mandate_from_artifact(self, &task.id)?;
        let authority = ResidentAuthority::from_history(history);
        let open_requests = view::open_grant_requests(self.get_task_decision_requests(&task.id)?);
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
                        Some(view::to_json(&record))
                    }
                    _ => None,
                },
            );
        let live_cycle = cycle::live_cycle_from_history(&history)?
            .filter(|live| live.expires_at > chrono::Utc::now());
        let (state, reason) = view.state();
        let cycle_routine_disabled = self.resident_cycle_routine_disabled();
        let stall =
            self.resident_stall_of(&view, chrono::Utc::now(), cycle_routine_disabled.as_deref())?;
        let mut value = json!({
            "task_id": view.task.id,
            "control": view::control_json(&view),
            "cycle_routine_disabled": cycle_routine_disabled.is_some(),
            "cycle_routine_name": cycle_routine_disabled,
            "stalled": stall.is_some(),
            "stalled_reason": stall.map(|stall| stall.reason),
            "title": view.task.title,
            "status": view.task.status,
            "latest_checkpoint": latest_checkpoint,
            "live_cycle": live_cycle,
            "state": state,
            "mandate": view.mandate.as_ref().ok().map(view::to_json),
            "effective_authority": view::to_json(&view.effective()),
            "authority": view.authority.as_ref().ok().map(view::to_json),
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
        let cycle_routine_disabled = self.resident_cycle_routine_disabled();
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
            let stall = self.resident_stall_of(
                &view,
                chrono::Utc::now(),
                cycle_routine_disabled.as_deref(),
            )?;
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
                "cycle_routine_disabled": cycle_routine_disabled.is_some(),
                "cycle_routine_name": cycle_routine_disabled,
                "stalled": stall.is_some(),
                "stalled_reason": stall.map(|stall| stall.reason),
            }));
        }
        Ok(json!({ "count": rows.len(), "mandates": rows }))
    }
}
