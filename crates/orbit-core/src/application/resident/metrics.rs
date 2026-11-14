//! `orbit resident metrics`: the four widening measures of design §9,
//! derived read-only from task history, decision requests, checkpoints and
//! audit rows. No new store: checkpoints and answers are assignment
//! comments, cycles and actions are assignment history, escalations are
//! decision requests.
//!
//! Definitions (design §9):
//! - manual interventions: human writes on the assignment or watched tasks
//!   that are not answers to a resident escalation and not grants —
//!   requeues, cancels, manual status changes, operator steering comments,
//!   unprompted relays (`resident.answer` without a `request_id`) and
//!   `stop`/`takeover` writes — per mandate and per active mandate-day.
//! - escalation load: resident-raised decision requests per mandate-day and
//!   per human-day, median answer latency, share answered with the
//!   recommended option, expired-unanswered count.
//! - reversal rate: share of resident decisions and actions a human undid
//!   within 7 days (the reversal window), each counted once.
//! - recovery cost: cycles that ended without an agent checkpoint, time
//!   from the dead cycle's start to the next good checkpoint, recovery
//!   cycle slot minutes, repeated actions and human interventions needed.
//!
//! `--baseline <task>` computes the same measures for an interactive
//! orchestration task; a measure it cannot derive is the string
//! `not_derivable`, never zero.
//!
//! A referenced task (watched, related, a checkpoint subject or request
//! task) that is not in this workspace is read through its registered
//! owner on this machine: its history and decision requests count, its
//! comments do not (no cross-workspace comment read exists). A reference
//! no registered workspace holds is listed as unreachable and skipped, so
//! one foreign or dangling id never fails the whole report [ORB-21642].

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Duration, Utc};
use orbit_common::{NotFoundKind, OrbitError};
use orbit_store::RegisteredTaskResolution;
use orbit_types::task::{DecisionRequestState, Task, TaskHistoryEntry, TaskStatus};
use orbit_types::workflow::{RESIDENT_TAG, ResidentComment};
use serde_json::{Value, json};

use super::due::{checkpoints, started_cycles};
use crate::OrbitRuntime;
use crate::application::statistics;

/// The reversal window of design §9: a human undo within 7 days of the
/// resident decision or action it undoes.
const REVERSAL_WINDOW_DAYS: i64 = 7;
/// Default metrics window when `--since` is absent.
const DEFAULT_WINDOW_DAYS: i64 = 30;

fn is_human_writer(by: &str) -> bool {
    let by = by.trim();
    by == "human" || by == "operator" || by.starts_with("human:") || by.starts_with("human-")
}

pub(super) fn parse_window_start(
    since: Option<&str>,
    now: DateTime<Utc>,
) -> Result<DateTime<Utc>, OrbitError> {
    let Some(raw) = since.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(now - Duration::days(DEFAULT_WINDOW_DAYS));
    };
    if let Ok(at) = DateTime::parse_from_rfc3339(raw) {
        return Ok(at.with_timezone(&Utc));
    }
    if let Ok(day) = chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d")
        && let Some(at) = day.and_hms_opt(0, 0, 0)
    {
        return Ok(DateTime::from_naive_utc_and_offset(at, Utc));
    }
    let compact: String = raw.chars().filter(|c| !c.is_whitespace()).collect();
    let compact = compact.trim_end_matches('s');
    let split = compact
        .find(|c: char| c.is_ascii_alphabetic())
        .ok_or_else(|| OrbitError::InvalidInput(format!("unrecognised `--since` window: {raw}")))?;
    let (amount, unit) = compact.split_at(split);
    let amount: i64 = amount
        .parse()
        .map_err(|_| OrbitError::InvalidInput(format!("unrecognised `--since` window: {raw}")))?;
    if amount < 0 {
        return Err(OrbitError::InvalidInput(format!(
            "unrecognised `--since` window: {raw}"
        )));
    }
    let span = match unit {
        "m" | "min" | "minute" => Duration::try_minutes(amount),
        "h" | "hour" => Duration::try_hours(amount),
        "d" | "day" => Duration::try_days(amount),
        "w" | "week" => Duration::try_weeks(amount),
        _ => {
            return Err(OrbitError::InvalidInput(format!(
                "unrecognised `--since` window: {raw} (use e.g. 7d, 24h, 2w or an RFC 3339 instant)"
            )));
        }
    };
    span.and_then(|span| now.checked_sub_signed(span))
        .ok_or_else(|| OrbitError::InvalidInput(format!("out of range `--since` window: {raw}")))
}

/// The median of `values` without reordering them: callers also report the
/// samples in the order they were recorded.
fn median_seconds(values: &[f64]) -> Option<f64> {
    statistics::median(&mut values.to_vec())
}

fn active_days(task_created: DateTime<Utc>, start: DateTime<Utc>, end: DateTime<Utc>) -> f64 {
    let from = task_created.max(start);
    let seconds = (end - from).num_seconds().max(0) as f64;
    (seconds / 86_400.0).ceil().max(1.0)
}

/// How one referenced task resolved for the metrics read.
enum MetricsReference {
    /// In this workspace: comments, history and decision requests count.
    Local,
    /// Held by another workspace registered on this machine: history and
    /// decision requests count; comments are not readable across workspaces.
    Foreign {
        task: Box<Task>,
        history: Vec<TaskHistoryEntry>,
        workspace: Option<String>,
    },
    /// No registered workspace holds it (or the id is malformed).
    Unreachable { reason: &'static str },
}

/// One mandate's (or one baseline task's) four measures over the window.
struct MeasureInputs {
    interventions: Vec<Value>,
    intervention_times: Vec<DateTime<Utc>>,
    escalations: Vec<orbit_types::task::DecisionRequest>,
    reversed: usize,
    reversal_total: usize,
    reversed_subjects: Vec<String>,
    crashed: usize,
    recovery_seconds: Vec<f64>,
    recovery_slot_minutes: f64,
    repeated_actions: usize,
    recovery_interventions: usize,
    has_checkpoints: bool,
    /// References read through their registered owner workspace.
    foreign_references: Vec<Value>,
    /// References no registered workspace could supply.
    unreachable_references: Vec<Value>,
}

impl OrbitRuntime {
    /// `orbit.resident.metrics`: per-mandate measures, workspace totals and
    /// an optional interactive baseline. Read-only.
    pub fn resident_metrics(
        &self,
        mandate: Option<&str>,
        since: Option<&str>,
        baseline: Option<&str>,
    ) -> Result<Value, OrbitError> {
        let now = Utc::now();
        let start = parse_window_start(since, now)?;
        let window = json!({
            "since": since,
            "start": start,
            "end": now,
            "days": ((now - start).num_seconds().max(0) as f64 / 86_400.0).ceil().max(1.0),
        });

        let mut mandate_ids: Vec<String> = Vec::new();
        if let Some(task_id) = mandate.map(str::trim).filter(|value| !value.is_empty()) {
            mandate_ids.push(self.get_task(task_id)?.id.to_string());
        } else if baseline.is_none() {
            for task in self.list_task_metadata()? {
                if task.tags.iter().any(|tag| tag == RESIDENT_TAG) {
                    mandate_ids.push(task.id.to_string());
                }
            }
            mandate_ids.sort();
            mandate_ids.dedup();
        }

        let mut mandates = Vec::new();
        let mut totals = MeasureInputs {
            interventions: Vec::new(),
            intervention_times: Vec::new(),
            escalations: Vec::new(),
            reversed: 0,
            reversal_total: 0,
            reversed_subjects: Vec::new(),
            crashed: 0,
            recovery_seconds: Vec::new(),
            recovery_slot_minutes: 0.0,
            repeated_actions: 0,
            recovery_interventions: 0,
            has_checkpoints: false,
            foreign_references: Vec::new(),
            unreachable_references: Vec::new(),
        };
        let mut mandate_days = 0.0f64;
        for task_id in &mandate_ids {
            let task = self.get_task(task_id)?;
            let days = active_days(task.created_at, start, now);
            mandate_days += days;
            let inputs = self.measure_task(task_id, start, now)?;
            totals.reversed += inputs.reversed;
            totals.reversal_total += inputs.reversal_total;
            totals.crashed += inputs.crashed;
            totals.recovery_seconds.extend(&inputs.recovery_seconds);
            totals.recovery_slot_minutes += inputs.recovery_slot_minutes;
            totals.repeated_actions += inputs.repeated_actions;
            totals.recovery_interventions += inputs.recovery_interventions;
            totals.has_checkpoints |= inputs.has_checkpoints;
            totals.intervention_times.extend(&inputs.intervention_times);
            mandates.push(mandate_json(task_id, days, &inputs, false));
            totals.interventions.extend(inputs.interventions);
            totals.escalations.extend(inputs.escalations);
            totals.foreign_references.extend(inputs.foreign_references);
            totals
                .unreachable_references
                .extend(inputs.unreachable_references);
        }

        let mut value = json!({
            "window": window,
            "mandates": mandates,
        });
        if mandate.is_some() || baseline.is_none() {
            let window_days = ((now - start).num_seconds().max(0) as f64 / 86_400.0)
                .ceil()
                .max(1.0);
            value["totals"] = totals_json(window_days, mandate_days, &totals);
        }
        if let Some(baseline_id) = baseline.map(str::trim).filter(|value| !value.is_empty()) {
            let task = self.get_task(baseline_id)?;
            let days = active_days(task.created_at, start, now);
            let inputs = self.measure_task(&task.id.to_string(), start, now)?;
            value["baseline"] = mandate_json(&task.id.to_string(), days, &inputs, true);
        }
        Ok(value)
    }

    /// Resolve a task the measures reference: this workspace first, then
    /// the registered owner on this machine (read-only, the dependency
    /// route), else unreachable. Only a not-found or malformed id degrades
    /// to unreachable; any other read failure still fails the report.
    fn metrics_reference(&self, id: &str) -> Result<MetricsReference, OrbitError> {
        match self.get_task(id) {
            Ok(_) => return Ok(MetricsReference::Local),
            Err(OrbitError::NotFound {
                kind: NotFoundKind::Task,
                ..
            }) => {}
            Err(OrbitError::InvalidInput(_)) => {
                return Ok(MetricsReference::Unreachable {
                    reason: "invalid_id",
                });
            }
            Err(error) => return Err(error),
        }
        let task = match self.resolve_dependency_task(id)? {
            RegisteredTaskResolution::Resolved(task) => task,
            RegisteredTaskResolution::Missing => {
                return Ok(MetricsReference::Unreachable { reason: "missing" });
            }
            RegisteredTaskResolution::ForeignAuthority => {
                return Ok(MetricsReference::Unreachable {
                    reason: "foreign_authority",
                });
            }
        };
        let history = self.get_dependency_history(id)?.unwrap_or_default();
        let workspace = self.registered_task_workspace(id)?;
        Ok(MetricsReference::Foreign {
            task,
            history,
            workspace,
        })
    }

    /// The four measures for one assignment task (resident or interactive).
    fn measure_task(
        &self,
        task_id: &str,
        start: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<MeasureInputs, OrbitError> {
        let task = self.get_task(task_id)?;
        let history = self.get_task_history(task_id)?;
        let comments = self.get_task_comments(task_id)?;
        let parsed_checkpoints = checkpoints(task_id, &comments);
        // Checkpoint timestamps come from their comments, oldest first.
        let mut checkpoint_times: Vec<(DateTime<Utc>, usize)> = Vec::new();
        {
            let mut index = 0usize;
            for comment in &comments {
                if let Ok(ResidentComment::Checkpoint(record)) =
                    ResidentComment::parse(&comment.message)
                    && record.mandate == task_id
                {
                    checkpoint_times.push((comment.at, index));
                    index += 1;
                }
            }
            let _ = &parsed_checkpoints;
        }
        let checkpoint_at = |index: usize| -> Option<DateTime<Utc>> {
            checkpoint_times
                .iter()
                .find(|(_, i)| *i == index)
                .map(|(at, _)| *at)
        };

        // Watched tasks: the mandate's watch plus checkpoint action subjects.
        let mut watched: BTreeSet<String> = [task_id.to_string()].into_iter().collect();
        if let Ok(view) = self.resident_view(task.clone(), &history)
            && let Ok(mandate) = &view.mandate
        {
            watched.extend(mandate.watch.tasks.iter().cloned());
            if !mandate.watch.tags.is_empty() {
                for other in self.list_task_metadata()? {
                    if other
                        .tags
                        .iter()
                        .any(|tag| mandate.watch.tags.contains(tag))
                    {
                        watched.insert(other.id.to_string());
                    }
                }
            }
        }
        for checkpoint in &parsed_checkpoints {
            for action in &checkpoint.actions {
                watched.insert(action.subject.clone());
            }
        }
        for relation in &task.relations {
            watched.insert(relation.target.to_string());
        }

        // --- Manual interventions -------------------------------------
        let mut interventions: Vec<Value> = Vec::new();
        let mut intervention_times: Vec<DateTime<Utc>> = Vec::new();
        let record_intervention =
            |task_id: &str,
             at: DateTime<Utc>,
             by: &str,
             kind: &str,
             interventions: &mut Vec<Value>,
             times: &mut Vec<DateTime<Utc>>| {
                if at < start || at > now {
                    return;
                }
                interventions.push(json!({
                    "task_id": task_id, "at": at, "by": by, "kind": kind,
                }));
                times.push(at);
            };
        // Every referenced task, resolved once: watched tasks plus the
        // escalation tasks gathered below.
        let mut references: BTreeMap<String, MetricsReference> = BTreeMap::new();
        for id in &watched {
            if id != task_id {
                references.insert(id.clone(), self.metrics_reference(id)?);
            }
        }
        for watched_id in &watched {
            let watched_history = match references.get(watched_id) {
                None | Some(MetricsReference::Local) => None,
                Some(MetricsReference::Foreign { history, .. }) => Some(history.clone()),
                Some(MetricsReference::Unreachable { .. }) => continue,
            };
            let local = watched_history.is_none();
            let comments_read = if local {
                self.get_task_comments(watched_id)?
            } else {
                Vec::new()
            };
            for comment in comments_read {
                match ResidentComment::parse(&comment.message) {
                    // An unprompted relay is an intervention whoever relayed
                    // it; an answer to a request is not.
                    Ok(ResidentComment::Answer(answer)) => {
                        if answer.request_id.is_none() {
                            record_intervention(
                                watched_id,
                                comment.at,
                                &comment.by,
                                "unprompted_relay",
                                &mut interventions,
                                &mut intervention_times,
                            );
                        }
                    }
                    Ok(ResidentComment::Checkpoint(_) | ResidentComment::Request(_)) => {}
                    Err(_) => {
                        if is_human_writer(&comment.by) {
                            record_intervention(
                                watched_id,
                                comment.at,
                                &comment.by,
                                "comment",
                                &mut interventions,
                                &mut intervention_times,
                            );
                        }
                    }
                }
            }
            let history_read = match watched_history {
                Some(history) => history,
                None => self.get_task_history(watched_id)?,
            };
            for entry in history_read {
                if !is_human_writer(&entry.by) {
                    continue;
                }
                // Answers to escalations and authority grants are not
                // interventions.
                if entry.event == "decision_answered"
                    || entry.event.starts_with("resident_")
                    || entry.event == "decision_requested"
                {
                    continue;
                }
                let status_change = entry.event != "created"
                    && (entry.from_status.is_some() || entry.to_status.is_some());
                let named = entry.event.contains("requeue")
                    || entry.event.contains("cancel")
                    || entry.event.contains("stop")
                    || entry.event.contains("takeover");
                if status_change || named {
                    let kind = if named {
                        entry.event.as_str()
                    } else {
                        "status_change"
                    };
                    record_intervention(
                        watched_id,
                        entry.at,
                        &entry.by,
                        kind,
                        &mut interventions,
                        &mut intervention_times,
                    );
                }
            }
        }
        interventions.sort_by(|a, b| a["at"].as_str().cmp(&b["at"].as_str()));
        intervention_times.sort();

        // --- Escalation load ------------------------------------------
        let mut resident_request_ids: BTreeSet<String> = BTreeSet::new();
        for checkpoint in &parsed_checkpoints {
            for request in &checkpoint.requests {
                resident_request_ids.insert(request.decision_request.clone());
            }
        }
        for comment in &comments {
            if let Ok(ResidentComment::Request(request)) = ResidentComment::parse(&comment.message)
            {
                resident_request_ids.insert(request.decision_request.clone());
            }
        }
        // Escalation tasks: tasks related to the assignment, plus every task
        // a checkpoint or request record names.
        let mut escalation_task_ids: BTreeSet<String> = watched.clone();
        for other in self.list_task_metadata()? {
            if other
                .relations
                .iter()
                .any(|relation| relation.target == task_id)
            {
                escalation_task_ids.insert(other.id.to_string());
            }
        }
        for checkpoint in &parsed_checkpoints {
            for request in &checkpoint.requests {
                escalation_task_ids.insert(request.task.clone());
            }
        }
        for id in &escalation_task_ids {
            if id != task_id && !references.contains_key(id) {
                references.insert(id.clone(), self.metrics_reference(id)?);
            }
        }
        let mut escalations: Vec<orbit_types::task::DecisionRequest> = Vec::new();
        let mut seen_requests: BTreeSet<String> = BTreeSet::new();
        for id in &escalation_task_ids {
            let requests = match references.get(id) {
                None | Some(MetricsReference::Local) => self.get_task_decision_requests(id)?,
                // A foreign task's requests come from its owner-read record
                // and history; its comments are not readable here.
                Some(MetricsReference::Foreign { task, history, .. }) => {
                    self.task_decision_requests_in(task, history, &[], now)?
                }
                Some(MetricsReference::Unreachable { .. }) => continue,
            };
            for request in requests {
                let resident_raised = resident_request_ids.contains(&request.id)
                    || request.gate == orbit_types::task::DecisionGate::AgentRequest
                        && escalation_task_ids.contains(&request.task_id)
                        && request.task_id != task_id;
                if resident_raised && seen_requests.insert(request.id.clone()) {
                    escalations.push(request);
                }
            }
        }
        // A resident request record is authoritative even when its task read
        // resolves through the assignment itself.
        escalations.retain(|request| request.raised_at >= start && request.raised_at <= now);
        escalations.sort_by_key(|request| request.raised_at);

        // --- Reversal rate --------------------------------------------
        let answers: Vec<(DateTime<Utc>, String)> = comments
            .iter()
            .filter_map(
                |comment| match ResidentComment::parse(&comment.message).ok()? {
                    ResidentComment::Answer(answer) => Some((comment.at, answer.reverses.clone()?)),
                    _ => None,
                },
            )
            .collect();
        // Human status writes per subject task, read once.
        let mut subject_histories: BTreeMap<String, Vec<orbit_types::task::TaskHistoryEntry>> =
            BTreeMap::new();
        for checkpoint in &parsed_checkpoints {
            for action in &checkpoint.actions {
                if let std::collections::btree_map::Entry::Vacant(slot) =
                    subject_histories.entry(action.subject.clone())
                {
                    let history = match references.get(&action.subject) {
                        Some(MetricsReference::Foreign { history, .. }) => history.clone(),
                        Some(MetricsReference::Unreachable { .. }) => Vec::new(),
                        None | Some(MetricsReference::Local) => {
                            self.get_task_history(&action.subject).unwrap_or_default()
                        }
                    };
                    slot.insert(history);
                }
            }
        }
        let mut reversal_total = 0usize;
        let mut reversed = 0usize;
        let mut reversed_subjects: Vec<String> = Vec::new();
        for (index, checkpoint) in parsed_checkpoints.iter().enumerate() {
            let Some(at) = checkpoint_at(index) else {
                continue;
            };
            if at < start {
                continue;
            }
            let window_end = at + Duration::days(REVERSAL_WINDOW_DAYS);
            for decision in &checkpoint.decisions {
                reversal_total += 1;
                let undone = answers.iter().any(|(answer_at, reverses)| {
                    reverses == &decision.id && *answer_at > at && *answer_at <= window_end
                });
                if undone {
                    reversed += 1;
                    reversed_subjects.push(decision.id.clone());
                }
            }
            for action in &checkpoint.actions {
                if action.result != "ok" {
                    continue;
                }
                reversal_total += 1;
                let by_answer = answers.iter().any(|(answer_at, reverses)| {
                    reverses == &action.key && *answer_at > at && *answer_at <= window_end
                });
                let by_write = subject_histories
                    .get(&action.subject)
                    .into_iter()
                    .flatten()
                    .any(|entry| {
                        is_human_writer(&entry.by)
                            && entry.at > at
                            && entry.at <= window_end
                            && (matches!(
                                entry.to_status,
                                Some(TaskStatus::Proposed | TaskStatus::Rejected)
                            ) || entry.event.contains("revert"))
                    });
                if by_answer || by_write {
                    reversed += 1;
                    reversed_subjects.push(action.key.clone());
                }
            }
        }

        // --- Recovery cost --------------------------------------------
        let cycles = started_cycles(&history);
        let cycle_start: BTreeMap<String, DateTime<Utc>> = cycles
            .iter()
            .map(|cycle| (cycle.live.cycle.clone(), cycle.at))
            .collect();
        let timed_checkpoints: Vec<(DateTime<Utc>, &orbit_types::workflow::ResidentCheckpoint)> =
            parsed_checkpoints
                .iter()
                .enumerate()
                .filter_map(|(index, checkpoint)| Some((checkpoint_at(index)?, checkpoint)))
                .collect();
        let mut crashed = 0usize;
        let mut recovery_seconds: Vec<f64> = Vec::new();
        let mut recovery_interventions = 0usize;
        for (at, checkpoint) in &timed_checkpoints {
            if checkpoint.outcome != "incomplete" || *at < start {
                continue;
            }
            crashed += 1;
            let dead_start = cycle_start.get(&checkpoint.cycle).copied().unwrap_or(*at);
            let next_good = timed_checkpoints
                .iter()
                .filter(|(good_at, good)| good.outcome != "incomplete" && *good_at > *at)
                .map(|(good_at, _)| *good_at)
                .min();
            if let Some(good_at) = next_good {
                recovery_seconds.push((good_at - dead_start).num_seconds().max(0) as f64);
            }
            let until = next_good.unwrap_or(now);
            recovery_interventions += intervention_times
                .iter()
                .filter(|time| **time > dead_start && **time <= until)
                .count();
        }
        let mut recovery_slot_minutes = 0.0f64;
        let mut repeated_actions = 0usize;
        let mut ok_keys: BTreeSet<String> = BTreeSet::new();
        for (at, checkpoint) in &timed_checkpoints {
            for action in &checkpoint.actions {
                if action.result == "ok" && !ok_keys.insert(action.key.clone()) {
                    repeated_actions += 1;
                }
            }
            if *at < start {
                continue;
            }
            if let Some(dead) = &checkpoint.recovery_of {
                if let Some(recovery_start) = cycle_start.get(&checkpoint.cycle) {
                    recovery_slot_minutes +=
                        (*at - *recovery_start).num_seconds().max(0) as f64 / 60.0;
                }
                // A completed dead-cycle action run again (instead of the
                // ledger's `already_done`) is a repeat.
                let dead_prefix = format!("resident:{task_id}:{dead}:");
                repeated_actions += checkpoint
                    .actions
                    .iter()
                    .filter(|action| action.result == "ok" && action.key.starts_with(&dead_prefix))
                    .count();
            }
        }

        let mut foreign_references = Vec::new();
        let mut unreachable_references = Vec::new();
        for (id, reference) in &references {
            match reference {
                MetricsReference::Local => {}
                MetricsReference::Foreign { workspace, .. } => foreign_references.push(json!({
                    "task_id": id,
                    "workspace": workspace,
                    "read": ["history", "decision_requests"],
                    "not_read": ["comments"],
                })),
                MetricsReference::Unreachable { reason } => unreachable_references.push(json!({
                    "task_id": id,
                    "reason": reason,
                })),
            }
        }

        Ok(MeasureInputs {
            interventions,
            intervention_times,
            escalations,
            reversed,
            reversal_total,
            reversed_subjects,
            crashed,
            recovery_seconds,
            recovery_slot_minutes,
            repeated_actions,
            recovery_interventions,
            has_checkpoints: !parsed_checkpoints.is_empty(),
            foreign_references,
            unreachable_references,
        })
    }
}

fn escalation_json(escalations: &[orbit_types::task::DecisionRequest], active_days: f64) -> Value {
    let answered: Vec<_> = escalations
        .iter()
        .filter(|request| request.state == DecisionRequestState::Answered)
        .collect();
    let latencies: Vec<f64> = answered
        .iter()
        .filter_map(|request| {
            request.resolution.as_ref().map(|resolution| {
                (resolution.answered_at - request.raised_at)
                    .num_seconds()
                    .max(0) as f64
            })
        })
        .collect();
    let recommended_answered = answered
        .iter()
        .filter(|request| {
            request.recommended.is_some()
                && request.resolution.as_ref().map(|r| &r.answer_id) == request.recommended.as_ref()
        })
        .count();
    let with_recommendation = answered
        .iter()
        .filter(|request| request.recommended.is_some())
        .count();
    json!({
        "count": escalations.len(),
        "total": escalations.len(),
        "per_mandate_day": escalations.len() as f64 / active_days,
        "per_human_day": escalations.len() as f64 / active_days,
        "answered": answered.len(),
        "open": escalations.iter().filter(|r| r.state == DecisionRequestState::Open).count(),
        "expired_unanswered": escalations.iter().filter(|r| r.state == DecisionRequestState::Expired).count(),
        "median_answer_latency_seconds": median_seconds(&latencies),
        "answer_latency_seconds": latencies,
        "recommended_share": if with_recommendation > 0 {
            json!(recommended_answered as f64 / with_recommendation as f64)
        } else {
            Value::Null
        },
        "share_answered_with_recommended": if with_recommendation > 0 {
            json!(recommended_answered as f64 / with_recommendation as f64)
        } else {
            Value::Null
        },
        "requests": escalations.iter().map(|r| r.id.clone()).collect::<Vec<_>>(),
    })
}

fn reversal_json(inputs: &MeasureInputs) -> Value {
    json!({
        "total": inputs.reversal_total,
        "decisions_and_actions": inputs.reversal_total,
        "reversed": inputs.reversed,
        "reversal_count": inputs.reversed,
        "rate": if inputs.reversal_total > 0 {
            json!(inputs.reversed as f64 / inputs.reversal_total as f64)
        } else {
            Value::Null
        },
        "reversal_rate": if inputs.reversal_total > 0 {
            json!(inputs.reversed as f64 / inputs.reversal_total as f64)
        } else {
            Value::Null
        },
        "reversal_window_days": REVERSAL_WINDOW_DAYS,
        "reversed_subjects": inputs.reversed_subjects,
    })
}

fn recovery_json(inputs: &MeasureInputs) -> Value {
    json!({
        "crashed_cycles": inputs.crashed,
        "cycles_without_checkpoint": inputs.crashed,
        "incomplete_checkpoints": inputs.crashed,
        "time_to_next_good_checkpoint_seconds": {
            "median": median_seconds(&inputs.recovery_seconds),
            "max": inputs.recovery_seconds.iter().copied().fold(None::<f64>, |acc, v| {
                Some(acc.map_or(v, |a: f64| a.max(v)))
            }),
            "samples": inputs.recovery_seconds,
        },
        "median_time_to_next_good_checkpoint_seconds": median_seconds(&inputs.recovery_seconds),
        "recovery_slot_minutes": inputs.recovery_slot_minutes,
        "repeated_actions": inputs.repeated_actions,
        "human_interventions_needed": inputs.recovery_interventions,
    })
}

fn mandate_json(task_id: &str, active_days: f64, inputs: &MeasureInputs, baseline: bool) -> Value {
    let interventions = json!({
        "count": inputs.interventions.len(),
        "total": inputs.interventions.len(),
        "per_mandate": inputs.interventions.len(),
        "per_mandate_day": inputs.interventions.len() as f64 / active_days,
        "active_days": active_days,
        "items": inputs.interventions,
    });
    let escalations = escalation_json(&inputs.escalations, active_days);
    let references = json!({
        "foreign": inputs.foreign_references,
        "unreachable": inputs.unreachable_references,
    });
    if baseline && !inputs.has_checkpoints {
        // An interactive loop leaves no checkpoint decisions and no cycle
        // record: reversals and crash recovery cannot be derived.
        return json!({
            "task_id": task_id,
            "baseline": true,
            "active_days": active_days,
            "interventions": interventions,
            "manual_interventions": interventions,
            "escalations": escalations,
            "escalation_load": escalations,
            "reversals": "not_derivable",
            "reversal_rate": "not_derivable",
            "recovery": "not_derivable",
            "recovery_cost": "not_derivable",
            "references": references,
        });
    }
    let reversals = reversal_json(inputs);
    let recovery = recovery_json(inputs);
    json!({
        "task_id": task_id,
        "active_days": active_days,
        "interventions": interventions.clone(),
        "manual_interventions": interventions,
        "escalations": escalations.clone(),
        "escalation_load": escalations,
        "reversals": reversals.clone(),
        "reversal_rate": reversals,
        "recovery": recovery.clone(),
        "recovery_cost": recovery,
        "references": references,
    })
}

fn totals_json(window_days: f64, mandate_days: f64, totals: &MeasureInputs) -> Value {
    // Workspace totals divide per-mandate-day rates by summed mandate active
    // days (mandate-days), not by the calendar window. Per-human-day rates
    // stay over the calendar window.
    let window_days = window_days.max(1.0);
    let mandate_days = mandate_days.max(1.0);
    let mut aggregate = mandate_json("totals", mandate_days, totals, false);
    let per_human_day = totals.escalations.len() as f64 / window_days;
    aggregate["escalations"]["per_human_day"] = json!(per_human_day);
    aggregate["escalation_load"]["per_human_day"] = json!(per_human_day);
    aggregate["task_id"] = Value::Null;
    aggregate
}
