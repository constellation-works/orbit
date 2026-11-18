//! Runtime reads of resident mandates and their current state.

use std::collections::{BTreeMap, BTreeSet};

use orbit_common::OrbitError;
use orbit_types::task::{DecisionRequestState, Task, TaskComment, TaskHistoryEntry};
use orbit_types::workflow::{
    RESIDENT_CYCLE_GRANTED_EVENT, RESIDENT_TAG, ResidentAuthority, ResidentComment,
    ResidentCycleGrant, ResidentMandate, ResidentRight,
};
use serde_json::{Map, Value, json};

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
    /// the cause, since no cycle can fire until it is enabled — and it
    /// stalls the mandate in its own right, even one that checkpointed
    /// recently, because no cycle will ever fire for it until the routine
    /// is enabled or the mandate is stopped.
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
        if stall.is_none()
            && let Some(routine_name) = cycle_routine_disabled
        {
            let last_checkpoint_at = comments.iter().rev().find_map(|comment| {
                match ResidentComment::parse(&comment.message).ok()? {
                    ResidentComment::Checkpoint(record)
                        if record.mandate == view.task.id.as_str() =>
                    {
                        Some(comment.at)
                    }
                    _ => None,
                }
            });
            stall = Some(due::ResidentStall {
                reason: resident_cycle_routine_disabled_reason(routine_name),
                since: last_checkpoint_at.unwrap_or(view.task.created_at),
                last_checkpoint_at,
            });
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

    /// [`Self::resident_view`] over records the caller already loaded, so a
    /// list read does not re-read each mandate's bundle for its decisions.
    pub(crate) fn resident_view_in(
        &self,
        task: Task,
        history: &[TaskHistoryEntry],
        comments: &[TaskComment],
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(ResidentView, Vec<orbit_types::task::DecisionRequest>), OrbitError> {
        let mandate = resident_mandate_from_artifact(self, &task.id)?;
        let authority = ResidentAuthority::from_history(history);
        let requests = self.task_decision_requests_in(&task, history, comments, now)?;
        let open_requests = view::open_grant_requests(requests.clone());
        let control = control::resident_control_from_history(history);
        Ok((
            ResidentView {
                task,
                mandate,
                authority,
                open_requests,
                control,
            },
            requests,
        ))
    }

    pub(crate) fn resident_stall_in(
        &self,
        view: &ResidentView,
        comments: &[TaskComment],
        now: chrono::DateTime<chrono::Utc>,
        cycle_routine_disabled: Option<&str>,
    ) -> Option<due::ResidentStall> {
        let Ok(mandate) = &view.mandate else {
            return None;
        };
        if view.state().0 != ResidentMandateState::Active {
            return None;
        }
        let mut stall = due::resident_stall(&view.task, mandate, comments, now);
        if let (Some(stall), Some(routine_name)) = (&mut stall, cycle_routine_disabled) {
            stall.reason = format!(
                "{}; {}",
                stall.reason,
                resident_cycle_routine_disabled_reason(routine_name)
            );
        }
        if stall.is_none()
            && let Some(routine_name) = cycle_routine_disabled
        {
            let last_checkpoint_at = comments.iter().rev().find_map(|comment| {
                match ResidentComment::parse(&comment.message).ok()? {
                    ResidentComment::Checkpoint(record)
                        if record.mandate == view.task.id.as_str() =>
                    {
                        Some(comment.at)
                    }
                    _ => None,
                }
            });
            stall = Some(due::ResidentStall {
                reason: resident_cycle_routine_disabled_reason(routine_name),
                since: last_checkpoint_at.unwrap_or(view.task.created_at),
                last_checkpoint_at,
            });
        }
        stall
    }

    /// The mandate's status line as the reads report it: the title and body
    /// of its notice, when it was last posted, when it lapses, and `stale`
    /// once it has lapsed or been closed. `null` before the first line, or
    /// when the notices cannot be read.
    fn resident_status_line_json(&self, task_id: &str) -> Value {
        match self.resident_status_line(task_id) {
            Ok(Some(notice)) => json!({
                "notice_id": notice.id,
                "title": notice.title,
                "body": notice.body,
                "posted_at": notice.updated_at,
                "expires_at": notice.expires_at,
                "status": notice.status,
                "stale": notice.status != orbit_types::record::EffectiveStatus::Open,
            }),
            Ok(None) => Value::Null,
            Err(error) => {
                tracing::warn!(task = task_id, %error, "failed to read a resident's status line");
                Value::Null
            }
        }
    }

    /// `orbit.resident.show`: the mandate, its effective authority, the
    /// authority record with its grant history, open grant requests, live
    /// cycle, latest parsed checkpoint and the mandate's state.
    pub fn show_resident_mandate(&self, task_id: &str) -> Result<Value, OrbitError> {
        let row = self.get_task_row(task_id.trim())?;
        let now = chrono::Utc::now();
        let (view, _requests) =
            self.resident_view_in(row.task, &row.history, &row.comments, now)?;
        let cycle_routine_disabled = self.resident_cycle_routine_disabled();
        let stall =
            self.resident_stall_in(&view, &row.comments, now, cycle_routine_disabled.as_deref());
        let status_line = self.resident_status_line_json(&view.task.id);
        resident_show_json(
            &view,
            &row.comments,
            &row.history,
            stall,
            cycle_routine_disabled,
            status_line,
            now,
        )
    }

    /// Every mandate as [`Self::show_resident_mandate`] reports it, plus the
    /// card reads (`goal`, `open_decisions`, `cycle_authority`), in one pass:
    /// one bundle read and one notice scan per list, not per card phase.
    pub fn list_resident_mandate_cards(
        &self,
        include_ended: bool,
    ) -> Result<Vec<Value>, OrbitError> {
        let now = chrono::Utc::now();
        let cycle_routine_disabled = self.resident_cycle_routine_disabled();
        let mut tasks: Vec<Task> = Vec::new();
        for task in self.list_task_metadata()? {
            if !task.tags.iter().any(|tag| tag == RESIDENT_TAG) {
                continue;
            }
            if !include_ended && resident_mandate_ended(task.status) {
                continue;
            }
            tasks.push(task);
        }
        let ids: Vec<String> = tasks.iter().map(|task| task.id.clone()).collect();
        let status_lines = self.resident_status_lines(&ids).unwrap_or_else(|error| {
            tracing::warn!(%error, "resident cards could not read status lines");
            Default::default()
        });
        struct Card {
            shown: Value,
            task_id: String,
            history: Vec<TaskHistoryEntry>,
            requests: Vec<orbit_types::task::DecisionRequest>,
        }
        let mut cards: Vec<Card> = Vec::new();
        let mut requests_by_task: BTreeMap<String, Vec<orbit_types::task::DecisionRequest>> =
            BTreeMap::new();
        for task in tasks {
            let task_id = task.id.clone();
            let row = match self.get_task_row(&task_id) {
                Ok(row) => row,
                Err(error) => {
                    tracing::warn!(%error, %task_id, "resident card skipped an unreadable mandate");
                    continue;
                }
            };
            let (view, requests) =
                self.resident_view_in(row.task, &row.history, &row.comments, now)?;
            let stall = self.resident_stall_in(
                &view,
                &row.comments,
                now,
                cycle_routine_disabled.as_deref(),
            );
            let status_line = status_lines.get(&task_id).map_or(Value::Null, |notice| {
                json!({
                    "notice_id": notice.id,
                    "title": notice.title,
                    "body": notice.body,
                    "posted_at": notice.updated_at,
                    "expires_at": notice.expires_at,
                    "status": notice.status,
                    "stale": notice.status != orbit_types::record::EffectiveStatus::Open,
                })
            });
            let mut shown = resident_show_json(
                &view,
                &row.comments,
                &row.history,
                stall,
                cycle_routine_disabled.clone(),
                status_line,
                now,
            )?;
            shown["goal"] = view
                .mandate
                .as_ref()
                .ok()
                .map_or(Value::Null, |mandate| json!(mandate.goal.clone()));
            requests_by_task.insert(task_id.clone(), requests.clone());
            cards.push(Card {
                shown,
                task_id,
                history: row.history.clone(),
                requests,
            });
        }
        // Checkpoint decisions that live on other tasks, each read once.
        let mut extra: BTreeMap<String, Vec<orbit_types::task::DecisionRequest>> = BTreeMap::new();
        for card in &cards {
            for other in checkpoint_other_tasks(&card.shown, &card.task_id) {
                if requests_by_task.contains_key(&other) || extra.contains_key(&other) {
                    continue;
                }
                match self.get_task_decision_requests(&other) {
                    Ok(found) => {
                        extra.insert(other, found);
                    }
                    Err(error) => {
                        tracing::warn!(%error, task = %other, "resident card could not read a checkpoint decision");
                        extra.insert(other, Vec::new());
                    }
                }
            }
        }
        let mut mandates = Vec::new();
        for card in cards {
            let mut shown = card.shown;
            shown["open_decisions"] = open_decisions_json_for(
                &card.requests,
                &shown,
                &card.task_id,
                &requests_by_task,
                &extra,
            );
            shown["cycle_authority"] = cycle_authority_json_for(&shown, &card.history);
            mandates.push(shown);
        }
        Ok(mandates)
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
            let now = chrono::Utc::now();
            let row = self.get_task_row(&task.id)?;
            let (view, _requests) =
                self.resident_view_in(row.task, &row.history, &row.comments, now)?;
            let (state, reason) = view.state();
            let stall = self.resident_stall_in(
                &view,
                &row.comments,
                now,
                cycle_routine_disabled.as_deref(),
            );
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

fn resident_show_json(
    view: &ResidentView,
    comments: &[TaskComment],
    history: &[TaskHistoryEntry],
    stall: Option<due::ResidentStall>,
    cycle_routine_disabled: Option<String>,
    status_line: Value,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Value, OrbitError> {
    let latest_checkpoint =
        comments.iter().rev().find_map(|comment| {
            match ResidentComment::parse(&comment.message).ok()? {
                record @ ResidentComment::Checkpoint(_)
                    if record.mandate() == view.task.id.as_str() =>
                {
                    Some(view::to_json(&record))
                }
                _ => None,
            }
        });
    let live_cycle = cycle::live_cycle_from_history(history)?.filter(|live| live.expires_at > now);
    let (state, reason) = view.state();
    let mut value = json!({
        "task_id": view.task.id,
        "control": view::control_json(view),
        "cycle_routine_disabled": cycle_routine_disabled.is_some(),
        "cycle_routine_name": cycle_routine_disabled,
        "stalled": stall.is_some(),
        "stalled_reason": stall.map(|stall| stall.reason),
        "title": view.task.title,
        "status": view.task.status,
        "latest_checkpoint": latest_checkpoint,
        "status_line": status_line,
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

fn checkpoint_other_tasks(shown: &Value, task_id: &str) -> Vec<String> {
    let mut out = Vec::new();
    let Some(rows) = shown
        .get("latest_checkpoint")
        .and_then(|checkpoint| checkpoint.get("requests"))
        .and_then(Value::as_array)
    else {
        return out;
    };
    for row in rows {
        if row
            .get("decision_request")
            .and_then(Value::as_str)
            .is_none_or(|id| id.is_empty())
        {
            continue;
        }
        if let Some(other) = row
            .get("task")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty() && *id != task_id)
            && !out.iter().any(|id: &String| id == other)
        {
            out.push(other.to_string());
        }
    }
    out
}

fn open_decisions_json_for(
    own: &[orbit_types::task::DecisionRequest],
    shown: &Value,
    task_id: &str,
    by_task: &BTreeMap<String, Vec<orbit_types::task::DecisionRequest>>,
    extra: &BTreeMap<String, Vec<orbit_types::task::DecisionRequest>>,
) -> Value {
    let mut seen = BTreeSet::new();
    let mut values = Vec::new();
    let mut open: Vec<_> = own
        .iter()
        .filter(|request| request.state == DecisionRequestState::Open)
        .collect();
    open.sort_by(|a, b| (a.raised_at, &a.id).cmp(&(b.raised_at, &b.id)));
    for request in open {
        if seen.insert(request.id.clone()) {
            values.push(view::to_json(request));
        }
    }
    // Checkpoint decisions on other tasks, grouped as the web card did.
    let mut wanted: BTreeMap<String, Vec<String>> = BTreeMap::new();
    if let Some(rows) = shown
        .get("latest_checkpoint")
        .and_then(|checkpoint| checkpoint.get("requests"))
        .and_then(Value::as_array)
    {
        for row in rows {
            let Some(decision_id) = row
                .get("decision_request")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
            else {
                continue;
            };
            let Some(other_task) = row
                .get("task")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty() && *id != task_id)
            else {
                continue;
            };
            if seen.contains(decision_id) {
                continue;
            }
            let ids = wanted.entry(other_task.to_string()).or_default();
            if !ids.iter().any(|id| id == decision_id) {
                ids.push(decision_id.to_string());
            }
        }
    }
    for (other_task, decision_ids) in &wanted {
        let found = by_task.get(other_task).or_else(|| extra.get(other_task));
        let Some(found) = found else { continue };
        // The web card listed open decisions only; a checkpoint request that
        // has since closed must not reappear on the card.
        let mut matched: Vec<_> = found
            .iter()
            .filter(|request| {
                request.state == DecisionRequestState::Open
                    && decision_ids.iter().any(|id| id == &request.id)
            })
            .collect();
        matched.sort_by(|a, b| (a.raised_at, &a.id).cmp(&(b.raised_at, &b.id)));
        for request in matched {
            if seen.insert(request.id.clone()) {
                values.push(view::to_json(request));
            }
        }
    }
    Value::Array(values)
}

fn cycle_authority_json_for(shown: &Value, history: &[TaskHistoryEntry]) -> Value {
    let target_live = if let Some(cycle) = shown
        .get("live_cycle")
        .and_then(|live| live.get("cycle"))
        .and_then(Value::as_str)
        .filter(|cycle| !cycle.is_empty())
    {
        Some((cycle.to_string(), true))
    } else {
        shown
            .get("latest_checkpoint")
            .and_then(|checkpoint| checkpoint.get("cycle"))
            .and_then(Value::as_str)
            .filter(|cycle| !cycle.is_empty())
            .map(|cycle| (cycle.to_string(), false))
    };
    let Some((target, live)) = target_live else {
        return Value::Null;
    };
    if let Some(grant) = history.iter().rev().find_map(|entry| {
        if entry.event != RESIDENT_CYCLE_GRANTED_EVENT {
            return None;
        }
        let grant = serde_json::from_str::<ResidentCycleGrant>(entry.note.as_deref()?).ok()?;
        (grant.cycle == target).then_some(grant)
    }) {
        let rights: Vec<&str> = grant
            .rights
            .iter()
            .copied()
            .map(ResidentRight::as_str)
            .collect();
        let mut body = Map::new();
        body.insert("cycle".into(), json!(grant.cycle));
        body.insert("live".into(), json!(live));
        body.insert("rights".into(), json!(rights));
        body.insert("workspaces".into(), json!(grant.workspaces));
        body.insert("observe_only".into(), json!(grant.observe_only));
        if let Some(narrowed_by) = &grant.narrowed_by {
            body.insert("narrowed_by".into(), json!(narrowed_by));
        }
        return Value::Object(body);
    }
    if live {
        return Value::Null;
    }
    let Some(checkpoint) = shown.get("latest_checkpoint") else {
        return Value::Null;
    };
    if checkpoint.get("cycle").and_then(Value::as_str) != Some(target.as_str()) {
        return Value::Null;
    }
    let Some(authority) = checkpoint
        .get("authority")
        .filter(|value| value.is_object())
    else {
        return Value::Null;
    };
    let rights = authority
        .get("rights")
        .filter(|value| value.is_array())
        .cloned()
        .unwrap_or_else(|| json!([]));
    let mut body = Map::new();
    body.insert("cycle".into(), json!(target));
    body.insert("live".into(), json!(false));
    body.insert("rights".into(), rights);
    body.insert(
        "observe_only".into(),
        json!(
            authority
                .get("observe_only")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        ),
    );
    if let Some(narrowed_by) = authority.get("narrowed_by").and_then(Value::as_str) {
        body.insert("narrowed_by".into(), json!(narrowed_by));
    }
    body.insert("reported".into(), json!(true));
    Value::Object(body)
}
