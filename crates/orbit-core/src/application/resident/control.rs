//! The manager channel's writes: `orbit.resident.control` changes whether a
//! mandate fires, and `orbit.resident.relay` puts a human's words in front of
//! it verbatim.
//!
//! Control state lives in the assignment's history as
//! `resident_control` entries, each naming the action, the state it left,
//! the caller and the reason. Every action narrows or restores firing under
//! the existing authority; none can widen it. `resume`, `release` and `wake`
//! make the mandate due on the next tick (the cadence floor still applies),
//! and a relay is a wake event because its comment changes the wake digest.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_types::task::{TaskHistoryEntry, TaskStatus};
use orbit_types::workflow::{
    RESIDENT_TAG, ResidentAnswer, ResidentAnswerRelay, ResidentAnswerSource, ResidentComment,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::cycle::live_cycle_from_history;
use super::resident_mandate_ended;
use crate::OrbitRuntime;
use crate::application::task::{TaskRecordUpdateParams, TaskRecords, TaskUpdateParams};

/// History event recording one control action on a mandate.
pub(crate) const RESIDENT_CONTROL_EVENT: &str = "resident_control";

/// Longest relayed quote, in characters.
const RELAY_MAX_QUOTE_CHARS: usize = 4000;

/// One control action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResidentControlAction {
    /// Stop new cycles until `resume`.
    Pause,
    /// Restore firing after `pause`.
    Resume,
    /// End the mandate; the assignment moves to review once no cycle is live.
    Stop,
    /// Make an active mandate due on the next tick despite its heartbeat.
    Wake,
    /// A person takes over: nothing fires until `release`.
    Takeover,
    /// Hand the mandate back after `takeover`.
    Release,
}

impl ResidentControlAction {
    /// Parse a wire name.
    pub fn parse(name: &str) -> Option<Self> {
        serde_json::from_value(Value::String(name.trim().to_string())).ok()
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Pause => "pause",
            Self::Resume => "resume",
            Self::Stop => "stop",
            Self::Wake => "wake",
            Self::Takeover => "takeover",
            Self::Release => "release",
        }
    }

    /// Whether the action makes the mandate due on the next tick.
    fn wakes(self) -> bool {
        matches!(self, Self::Resume | Self::Wake | Self::Release)
    }
}

/// Whether a mandate may fire, as its control actions left it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResidentControlState {
    /// Fires when due.
    #[default]
    Active,
    /// Paused: no new cycle.
    Paused,
    /// Taken over by a person: no cycle fires.
    TakenOver,
    /// Stopped: the mandate has ended.
    Ended,
}

impl ResidentControlState {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Paused => "paused",
            Self::TakenOver => "taken_over",
            Self::Ended => "ended",
        }
    }
}

/// One `resident_control` history entry's note.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ResidentControlEntry {
    pub(crate) action: ResidentControlAction,
    pub(crate) state: ResidentControlState,
    pub(crate) reason: String,
    pub(crate) by: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) status_request: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) by_session: Option<String>,
}

/// The folded control record.
#[derive(Debug, Clone, Default)]
pub(crate) struct ResidentControl {
    pub(crate) state: ResidentControlState,
    /// The latest entry and when it was written.
    pub(crate) last: Option<(ResidentControlEntry, DateTime<Utc>)>,
    /// The latest waking action (`resume`, `wake`, `release`) and its time.
    pub(crate) last_wake: Option<(ResidentControlEntry, DateTime<Utc>)>,
    /// The latest waking action that requested a status checkpoint, and
    /// its time. Retained separately from `last_wake` so a later waking
    /// action without the flag does not overwrite an unconsumed request.
    pub(crate) last_status_request_wake: Option<(ResidentControlEntry, DateTime<Utc>)>,
}

impl ResidentControl {
    /// The waking action written after `since` (the last cycle start), if
    /// one is pending: the next cycle consumes it.
    pub(crate) fn pending_wake(
        &self,
        since: Option<DateTime<Utc>>,
    ) -> Option<&(ResidentControlEntry, DateTime<Utc>)> {
        self.last_wake
            .as_ref()
            .filter(|(_, at)| since.is_none_or(|since| *at > since))
    }

    /// Whether a status request written after `since` (the last cycle
    /// start) is still pending: the next cycle consumes it, so a request
    /// at or before `since` no longer counts.
    pub(crate) fn pending_status_request(&self, since: Option<DateTime<Utc>>) -> bool {
        self.last_status_request_wake
            .as_ref()
            .is_some_and(|(_, at)| since.is_none_or(|since| *at > since))
    }
}

/// Fold the control entries in a history. An unreadable entry is an error:
/// a record only core writes must not silently resume firing.
pub(crate) fn resident_control_from_history(
    history: &[TaskHistoryEntry],
) -> Result<ResidentControl, String> {
    let mut control = ResidentControl::default();
    for entry in history
        .iter()
        .filter(|entry| entry.event == RESIDENT_CONTROL_EVENT)
    {
        let note: ResidentControlEntry =
            serde_json::from_str(entry.note.as_deref().unwrap_or_default())
                .map_err(|error| format!("the resident control record is unreadable: {error}"))?;
        control.state = note.state;
        if note.action.wakes() {
            if note.status_request {
                control.last_status_request_wake = Some((note.clone(), entry.at));
            }
            control.last_wake = Some((note.clone(), entry.at));
        }
        control.last = Some((note, entry.at));
    }
    Ok(control)
}

/// The state `action` leaves from `from`, or why it does not apply.
fn transition(
    action: ResidentControlAction,
    from: ResidentControlState,
) -> Result<ResidentControlState, String> {
    use ResidentControlAction as A;
    use ResidentControlState as S;
    let to = match (action, from) {
        (_, S::Ended) => return Err("the mandate has ended".into()),
        (A::Pause, S::Active) => S::Paused,
        (A::Resume, S::Paused) => S::Active,
        (A::Stop, _) => S::Ended,
        (A::Wake, S::Active) => S::Active,
        (A::Takeover, S::Active | S::Paused) => S::TakenOver,
        (A::Release, S::TakenOver) => S::Active,
        (action, from) => {
            return Err(format!(
                "`{}` does not apply to a {} mandate",
                action.as_str(),
                from.as_str()
            ));
        }
    };
    Ok(to)
}

/// Who a manager-channel write is from, as the tool layer knows it.
#[derive(Debug, Clone, Default)]
pub struct ResidentChannelCaller {
    /// Agent identity the call carried.
    pub agent: Option<String>,
    /// Model the call carried.
    pub model: Option<String>,
    /// Orchestrator crew the session acts for.
    pub orchestrator: Option<String>,
}

/// One `orbit.resident.control` call.
#[derive(Debug, Clone)]
pub struct ResidentControlParams {
    /// The mandate's assignment task.
    pub task_id: String,
    /// What to do.
    pub action: ResidentControlAction,
    /// Why, recorded with the action.
    pub reason: String,
    /// With `wake`: the cycle checkpoints even when idle.
    pub status_request: bool,
    /// The session acting or taking over.
    pub by_session: Option<String>,
}

/// One `orbit.resident.relay` call.
#[derive(Debug, Clone)]
pub struct ResidentRelayParams {
    /// The mandate's assignment task.
    pub task_id: String,
    /// The human's words, verbatim.
    pub quote: String,
    /// Where the words came from.
    pub source: Value,
    /// The resident request or decision request this answers.
    pub request_id: Option<String>,
    /// The resident decision this direction reverses.
    pub reverses: Option<String>,
}

fn invalid(message: impl Into<String>) -> OrbitError {
    OrbitError::InvalidInput(message.into())
}

impl OrbitRuntime {
    /// `orbit.resident.control`: pause, resume, stop, wake, take over or
    /// release a mandate. Each action is one history entry with the caller.
    pub fn control_resident(
        &self,
        caller: &ResidentChannelCaller,
        params: ResidentControlParams,
    ) -> Result<Value, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        let reason = params.reason.trim().to_string();
        if reason.is_empty() {
            return Err(invalid("`reason` must not be empty"));
        }
        if params.status_request && params.action != ResidentControlAction::Wake {
            return Err(invalid("`status_request` applies only to `wake`"));
        }
        let task_id = params.task_id.trim().to_string();
        let writer = self
            .actor()
            .resolve_write_label(caller.agent.as_deref(), caller.model.as_deref())?;
        let mut written = None;
        self.stores()
            .tasks()
            .with_task_write_lock(&task_id, &mut || {
                let task = self.get_task(&task_id)?;
                if !task.tags.iter().any(|tag| tag == RESIDENT_TAG) {
                    return Err(invalid(format!(
                        "task {task_id} is not a resident mandate: tag it `{RESIDENT_TAG}`"
                    )));
                }
                let history = self.get_task_history(&task_id)?;
                let control =
                    resident_control_from_history(&history).map_err(OrbitError::Execution)?;
                let from = if resident_mandate_ended(task.status) {
                    ResidentControlState::Ended
                } else {
                    control.state
                };
                let to = transition(params.action, from).map_err(|reason| {
                    OrbitError::PolicyDenied(format!(
                        "resident_control_refused: {reason} (task {task_id})"
                    ))
                })?;
                let entry = ResidentControlEntry {
                    action: params.action,
                    state: to,
                    reason: reason.clone(),
                    by: writer.clone(),
                    status_request: params.status_request,
                    by_session: params
                        .by_session
                        .as_deref()
                        .map(str::trim)
                        .filter(|session| !session.is_empty())
                        .map(str::to_string),
                };
                let note = serde_json::to_string(&entry).map_err(|error| {
                    OrbitError::Execution(format!("serialize control: {error}"))
                })?;
                let at = Utc::now();
                self.stores().task_records().update(
                    &task_id,
                    TaskRecordUpdateParams {
                        actor: writer.clone(),
                        append_history: vec![TaskHistoryEntry {
                            at,
                            by: writer.clone(),
                            event: RESIDENT_CONTROL_EVENT.into(),
                            note: Some(note),
                            from_status: None,
                            to_status: None,
                            operation_id: None,
                            decision: None,
                        }],
                        ..Default::default()
                    },
                )?;
                written = Some((entry, at));
                Ok(())
            })?;
        let (entry, at) = written.ok_or_else(|| {
            OrbitError::Execution(format!("the control on task {task_id} was not recorded"))
        })?;
        let mut value = json!({
            "task_id": task_id,
            "action": entry.action,
            "state": entry.state,
            "effective_at": at,
            "by": entry.by,
        });
        if entry.action == ResidentControlAction::Stop {
            value["handoff"] = json!(self.hand_off_stopped_resident(&task_id)?);
        }
        Ok(value)
    }

    /// Move a stopped mandate's assignment to review with a synthesized
    /// handoff, unless a cycle is still live (its finalize does it then).
    /// A live cycle that has died is closed here instead: its synthesized
    /// `incomplete` checkpoint is written and its live entry cleared, so
    /// retrying this handoff recovers a stop whose cycle never finalized.
    /// Returns what happened: `review`, `cycle_live`, `already`,
    /// `not_stopped` or `refused: <why>` when the lifecycle does not allow
    /// the move (a `proposed` assignment, say); the operator moves it then.
    pub(crate) fn hand_off_stopped_resident(&self, task_id: &str) -> Result<String, OrbitError> {
        self.hand_off_stopped_resident_at(task_id, Utc::now())
    }

    pub(crate) fn hand_off_stopped_resident_at(
        &self,
        task_id: &str,
        now: DateTime<Utc>,
    ) -> Result<String, OrbitError> {
        let task = self.get_task(task_id)?;
        if task.status == TaskStatus::Review || resident_mandate_ended(task.status) {
            return Ok("already".into());
        }
        let history = self.get_task_history(task_id)?;
        let control = resident_control_from_history(&history).map_err(OrbitError::Execution)?;
        let Some((entry, at)) = control
            .last
            .filter(|(entry, _)| entry.action == ResidentControlAction::Stop)
        else {
            return Ok("not_stopped".into());
        };
        if let Some(live) = live_cycle_from_history(&history)? {
            if !self.resident_cycle_dead(&live, now)? {
                return Ok("cycle_live".into());
            }
            // The live cycle died before its finalizer could hand off:
            // close it here, with its synthesized final checkpoint, so a
            // later retry of this handoff recovers the stopped assignment
            // instead of relying on a finalizer that will never run.
            self.synthesize_resident_checkpoint(
                task_id,
                &live,
                "the stopped cycle died before finalization (process killed or lease expired)",
                None,
            )?;
            self.set_resident_live_cycle(task_id, Some(&live.cycle), None)?;
        }
        let summary = format!(
            "Resident stopped by {} at {}: {}\n\nNo further cycle runs. The latest checkpoint on this task is the resident's last state.",
            entry.by,
            at.to_rfc3339(),
            entry.reason
        );
        // Deterministic machinery: the lifecycle table still applies, but the
        // stopping cycle's own run (finalize runs inside it) must not block it.
        match self.update_task_as_system(
            task_id,
            TaskUpdateParams {
                status: Some(TaskStatus::Review),
                execution_summary: Some(summary),
                ..Default::default()
            },
            None,
        ) {
            Ok(_) => Ok("review".into()),
            Err(error) => Ok(format!("refused: {error}")),
        }
    }

    /// `orbit.resident.relay`: write one `resident.answer` comment carrying
    /// the human's words verbatim, with provenance and the relaying session.
    pub fn relay_to_resident(
        &self,
        caller: &ResidentChannelCaller,
        params: ResidentRelayParams,
    ) -> Result<Value, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        if params.quote.trim().is_empty() {
            return Err(invalid("`quote` must not be empty"));
        }
        if params.quote.chars().count() > RELAY_MAX_QUOTE_CHARS {
            return Err(invalid(format!(
                "`quote` must be at most {RELAY_MAX_QUOTE_CHARS} characters"
            )));
        }
        let source: ResidentAnswerSource =
            serde_json::from_value(params.source.clone()).map_err(|error| {
                invalid(format!(
                    "`source` must be {{human, via, session?, said_at}} and nothing else: {error}"
                ))
            })?;
        let task_id = params.task_id.trim().to_string();
        let task = self.get_task(&task_id)?;
        if !task.tags.iter().any(|tag| tag == RESIDENT_TAG) {
            return Err(invalid(format!(
                "task {task_id} is not a resident mandate: tag it `{RESIDENT_TAG}`"
            )));
        }
        let history = self.get_task_history(&task_id)?;
        let view = self.resident_view(task, &history)?;
        if view.state().0 == super::ResidentMandateState::Ended {
            return Err(OrbitError::PolicyDenied(format!(
                "task {task_id}'s mandate has ended, so it takes no relay"
            )));
        }
        let writer = self
            .actor()
            .resolve_write_label(caller.agent.as_deref(), caller.model.as_deref())?;
        let relayed_by = ResidentAnswerRelay {
            crew: caller
                .orchestrator
                .clone()
                .or_else(|| caller.agent.clone())
                .filter(|crew| !crew.trim().is_empty())
                .unwrap_or_else(|| writer.clone()),
            model: caller
                .model
                .clone()
                .filter(|model| !model.trim().is_empty())
                .unwrap_or_else(|| "none".into()),
        };
        let record = ResidentComment::Answer(ResidentAnswer {
            schema: 1,
            mandate: task_id.clone(),
            request_id: params.request_id,
            reverses: params.reverses,
            quote: params.quote,
            source,
            relayed_by: Some(relayed_by),
        });
        let message = serde_json::to_string(&record)
            .map_err(|error| OrbitError::Execution(format!("serialize relay: {error}")))?;
        ResidentComment::parse(&message).map_err(|error| invalid(error.to_string()))?;
        self.update_task_with_identity(
            &task_id,
            TaskUpdateParams {
                comment: Some(message.clone()),
                ..Default::default()
            },
            caller.agent.clone(),
            caller.model.clone(),
        )?;
        // Comments carry no ids: name the written one by its position.
        let comments = self.get_task_comments(&task_id)?;
        let comment = comments
            .iter()
            .enumerate()
            .rev()
            .find(|(_, comment)| comment.message == message)
            .map(
                |(index, comment)| json!({"index": index + 1, "at": comment.at, "by": comment.by}),
            );
        Ok(json!({
            "task_id": task_id,
            "comment": comment,
            "answer": record,
        }))
    }
}
