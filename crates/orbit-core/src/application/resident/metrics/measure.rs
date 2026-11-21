use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Duration, Utc};
use orbit_common::{NotFoundKind, OrbitError};
use orbit_store::RegisteredTaskResolution;
use orbit_types::task::{Task, TaskHistoryEntry, TaskStatus};
use orbit_types::workflow::{
    RESIDENT_MANDATE_ARTIFACT, RESIDENT_TAG, ResidentComment, ResidentMandate,
};
use serde_json::{Value, json};

use super::super::control::RESIDENT_CONTROL_EVENT;
use super::super::due::{checkpoints, started_cycles};
use super::report::MetricsReportKind;
use super::{report, window};
use crate::OrbitRuntime;

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
pub(super) struct MeasureInputs {
    pub(super) interventions: Vec<Value>,
    intervention_times: Vec<DateTime<Utc>>,
    pub(super) escalations: Vec<orbit_types::task::DecisionRequest>,
    pub(super) reversed: usize,
    pub(super) reversal_total: usize,
    pub(super) reversed_subjects: Vec<String>,
    pub(super) crashed: usize,
    pub(super) recovery_seconds: Vec<f64>,
    pub(super) recovery_slot_minutes: f64,
    pub(super) repeated_actions: usize,
    pub(super) recovery_interventions: usize,
    pub(super) has_checkpoints: bool,
    /// References read through their registered owner workspace.
    pub(super) foreign_references: Vec<Value>,
    /// References no registered workspace could supply.
    pub(super) unreachable_references: Vec<Value>,
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
        let start = window::parse_window_start(since, now)?;
        let window_days = ((now - start).num_seconds().max(0) as f64 / 86_400.0)
            .ceil()
            .max(1.0);
        let window = json!({
            "since": since,
            "start": start,
            "end": now,
            "days": window_days,
        });

        let explicit_mandate = mandate.map(str::trim).filter(|value| !value.is_empty());
        let mut mandate_ids: Vec<String> = Vec::new();
        if let Some(task_id) = explicit_mandate {
            let task = self.get_task(task_id)?;
            if self
                .get_task_artifact(&task.id, RESIDENT_MANDATE_ARTIFACT)?
                .is_some()
            {
                mandate_ids.push(task.id.to_string());
            }
        } else if baseline.is_none() {
            for task in self.list_task_metadata()? {
                if task.tags.iter().any(|tag| tag == RESIDENT_TAG)
                    && self
                        .get_task_artifact(&task.id, RESIDENT_MANDATE_ARTIFACT)?
                        .is_some()
                {
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
            let history = self.get_task_history(task_id)?;
            let comments = self.get_task_comments(task_id)?;
            let manifest_created_at = self
                .get_task_artifact_manifest(task_id)?
                .into_iter()
                .find(|file| file.path == RESIDENT_MANDATE_ARTIFACT)
                .map(|file| file.created_at);
            let parsed_mandate = self
                .get_task_artifact(task_id, RESIDENT_MANDATE_ARTIFACT)?
                .and_then(|artifact| ResidentMandate::parse(&artifact.content).ok());
            let activated_at =
                window::mandate_activated_at(&task, &history, &comments, manifest_created_at);
            let ended_at = window::mandate_ended_at(&task, &history, parsed_mandate.as_ref(), now);
            if explicit_mandate.is_none()
                && (ended_at.is_some_and(|end| end < start) || activated_at > now)
            {
                continue;
            }
            let span_start = activated_at.max(start);
            let span_end = ended_at.map_or(now, |end| end.min(now));
            let days = window::active_days(span_start, span_end);
            mandate_days += days;
            let inputs = self.measure_task(task_id, span_start, span_end, now)?;
            totals.reversed += inputs.reversed;
            totals.reversal_total += inputs.reversal_total;
            totals.crashed += inputs.crashed;
            totals.recovery_seconds.extend(&inputs.recovery_seconds);
            totals.recovery_slot_minutes += inputs.recovery_slot_minutes;
            totals.repeated_actions += inputs.repeated_actions;
            totals.recovery_interventions += inputs.recovery_interventions;
            totals.has_checkpoints |= inputs.has_checkpoints;
            totals.intervention_times.extend(&inputs.intervention_times);
            mandates.push(report::mandate_json(
                task_id,
                days,
                window_days,
                &inputs,
                MetricsReportKind::Mandate,
            ));
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
            value["totals"] = report::totals_json(window_days, mandate_days, &totals);
        }
        if let Some(baseline_id) = baseline.map(str::trim).filter(|value| !value.is_empty()) {
            let task = self.get_task(baseline_id)?;
            let history = self.get_task_history(&task.id)?;
            let active_start = task.created_at.max(start);
            let active_end =
                window::assignment_closed_at(&task, &history).map_or(now, |end| end.min(now));
            let days = window::active_days(active_start, active_end);
            let inputs = self.measure_task(&task.id.to_string(), active_start, active_end, now)?;
            value["baseline"] = report::mandate_json(
                &task.id.to_string(),
                days,
                window_days,
                &inputs,
                MetricsReportKind::Baseline,
            );
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

    /// The four measures for one assignment task (resident or interactive),
    /// bounded to `[span_start, span_end]` inside the report window ending at `now`.
    fn measure_task(
        &self,
        task_id: &str,
        span_start: DateTime<Utc>,
        span_end: DateTime<Utc>,
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
                if at < span_start || at > span_end {
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
                        if window::is_human_writer(&comment.by) {
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
                if !window::is_human_writer(&entry.by)
                    && !window::is_explicit_human_control_writer(&entry)
                {
                    continue;
                }
                // A human stop or takeover is an intervention. The other
                // control actions (wake, pause, resume, release) are not.
                if entry.event == RESIDENT_CONTROL_EVENT {
                    if let Some(action) = window::counted_control_action(&entry) {
                        record_intervention(
                            watched_id,
                            entry.at,
                            &entry.by,
                            action.as_str(),
                            &mut interventions,
                            &mut intervention_times,
                        );
                    }
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
        escalations
            .retain(|request| request.raised_at >= span_start && request.raised_at <= span_end);
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
            if at < span_start || at > span_end {
                continue;
            }
            let window_end = at + Duration::days(window::REVERSAL_WINDOW_DAYS);
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
                        window::is_human_writer(&entry.by)
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
            if checkpoint.outcome != "incomplete" || *at < span_start || *at > span_end {
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
            let until = next_good.unwrap_or(span_end);
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
            if *at < span_start || *at > span_end {
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
