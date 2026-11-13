//! The resident cycle's deterministic steps: `resident_observe` takes the
//! cycle lease, recovers a dead predecessor, records the cycle's grant and
//! builds the brief; `resident_finalize` closes the cycle, synthesizing an
//! `incomplete` checkpoint when the agent wrote none, and moves the assignment
//! to review when the agent checkpointed `handoff` or the mandate was stopped.

use chrono::{DateTime, Duration, Utc};
use orbit_common::OrbitError;
use orbit_types::task::{TaskComment, TaskHistoryEntry};
use orbit_types::workflow::{
    RESIDENT_BRIEF_CHECKPOINTS, RESIDENT_CYCLE_GRANTED_EVENT, RESIDENT_CYCLE_JOB,
    ResidentCheckpoint, ResidentCheckpointAction, ResidentCheckpointAuthority,
    ResidentCheckpointWake, ResidentComment, ResidentCycleGrant, ResidentLiveCycle,
    ResidentNextWake,
};
use serde_json::{Value, json};

use super::due::{checkpoints, cycle_grant, started_cycles};
use super::guard::action_rows;
use super::{ResidentMandateState, cycle::live_cycle_from_history};
use crate::OrbitRuntime;
use crate::application::task::{TaskRecordUpdateParams, TaskRecords};

/// How long a cycle's lease lasts: the agent step's budget plus margin.
pub const RESIDENT_CYCLE_LEASE_MINUTES: i64 = 40;

/// The cycle id a run takes.
pub fn resident_cycle_id(run_id: &str) -> String {
    format!("rcy-{run_id}")
}

fn skipped(task_id: &str, reason: &str) -> Value {
    json!({"task_id": task_id, "cycle_count": 0, "skip_reason": reason})
}

impl OrbitRuntime {
    /// The mandate a resident cycle run serves: the single task id the
    /// scheduler put in its input. Only a `resident_cycle_pipeline` run has one.
    pub(crate) fn resident_cycle_mandate_of_run(&self, run_id: &str) -> Result<String, OrbitError> {
        let run = self.show_job_run_observed(run_id)?;
        if run.job_id != RESIDENT_CYCLE_JOB {
            return Err(OrbitError::PolicyDenied(format!(
                "run {run_id} is not a {RESIDENT_CYCLE_JOB} run"
            )));
        }
        let ids: Vec<String> = run
            .input
            .as_ref()
            .and_then(|input| input.get("task_ids"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect();
        match ids.as_slice() {
            [id] => Ok(id.clone()),
            _ => Err(OrbitError::InvalidInput(
                "a resident cycle run names exactly one mandate in task_ids".into(),
            )),
        }
    }

    /// The cycle agent runs the mandate's crew, an explicit activity crew, while
    /// the run was admitted with the assignment task's own `crew` field. Record
    /// the crew that runs, so the run's Resolved Run Crew and recorded model
    /// match the agent's.
    fn record_resident_cycle_run_crew(
        &self,
        run_id: &str,
        mandate: &orbit_types::workflow::ResidentMandate,
    ) -> Result<(), OrbitError> {
        let crew = self.resolve_crew_for_task(Some(&mandate.crew), None)?;
        self.stores().jobs().record_job_run_crew(run_id, &crew)?;
        Ok(())
    }

    /// `resident_observe`: start this run's cycle for its mandate, or report
    /// why it does not (`cycle_count: 0`). A dead predecessor first gets its
    /// synthesized `incomplete` checkpoint, and this cycle recovers it.
    pub fn begin_resident_cycle(
        &self,
        run_id: &str,
        now: DateTime<Utc>,
    ) -> Result<Value, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        let task_id = self.resident_cycle_mandate_of_run(run_id)?;
        let task = self.get_task(&task_id)?;
        let history = self.get_task_history(&task_id)?;
        let view = self.resident_view(task.clone(), &history)?;
        if view.state().0 != ResidentMandateState::Active {
            // A stopped mandate never starts another cycle, but its live
            // cycle may have died before finalizing: recover that handoff
            // (final checkpoint, live entry cleared, assignment to review)
            // instead of only skipping.
            let mut result = skipped(&task_id, "mandate_not_active");
            if view.state().0 == ResidentMandateState::Ended {
                result["handoff"] = json!(self.hand_off_stopped_resident_at(&task_id, now)?);
            }
            if let Ok(Some(live)) = live_cycle_from_history(&history) {
                self.discard_resident_cycle_worktree(&live.run_id);
            }
            return Ok(result);
        }
        let Ok(mandate) = view.mandate.clone() else {
            return Ok(skipped(&task_id, "mandate_invalid"));
        };
        let effective = view.effective();
        let cycle = resident_cycle_id(run_id);
        let live = live_cycle_from_history(&history)?;
        // A resumed run re-enters the cycle it already holds.
        if let Some(live) = &live
            && live.run_id == run_id
            && let Some(grant) = cycle_grant(&history, &live.cycle)
        {
            self.record_resident_cycle_run_crew(run_id, &mandate)?;
            let worktree = self.create_resident_cycle_worktree(run_id);
            return self.resident_brief(&task_id, &mandate, &grant, &history, None, worktree);
        }
        let due = self.resident_due(&task, mandate.clone(), &history, now)?;
        // A `handoff` checkpoint whose finalize never ran (the agent step
        // failed after writing it) or whose move was refused: it ends the
        // cycles, so settle it instead of starting another one.
        if (live.is_none() || due.dead_cycle.is_some())
            && let Some(handoff) = self.settle_resident_handoff(&task_id)?
        {
            if let Some(dead) = &due.dead_cycle {
                self.set_resident_live_cycle(&task_id, Some(&dead.cycle), None)?;
                if dead.run_id != run_id {
                    self.discard_resident_cycle_worktree(&dead.run_id);
                }
            }
            let mut result = skipped(&task_id, "handoff_settled");
            result["handoff"] = json!(handoff);
            return Ok(result);
        }
        if let Some(reason) = &due.withheld {
            return Ok(skipped(&task_id, reason));
        }
        if due.reasons.is_empty() {
            return Ok(skipped(&task_id, "not_due"));
        }
        self.record_resident_cycle_run_crew(run_id, &mandate)?;
        let recovery_of = match &due.dead_cycle {
            Some(dead) => {
                self.synthesize_resident_checkpoint(
                    &task_id,
                    dead,
                    "the cycle died before it wrote a checkpoint (process killed or lease expired)",
                    None,
                )?;
                if dead.run_id != run_id {
                    self.discard_resident_cycle_worktree(&dead.run_id);
                }
                Some(dead.cycle.clone())
            }
            None => None,
        };
        let last_cycle_at_before_publication = due.last_cycle.as_ref().map(|cycle| cycle.at);
        let history_at_publication = self.publish_resident_live_cycle(
            &task_id,
            live.as_ref().map(|live| live.cycle.as_str()),
            &ResidentLiveCycle {
                cycle: cycle.clone(),
                run_id: run_id.to_string(),
                expires_at: now + Duration::minutes(RESIDENT_CYCLE_LEASE_MINUTES),
            },
        )?;
        // A wake committed after the observation above but before
        // publication is in this snapshot: its timestamp precedes the new
        // cycle's start, so the next cycle would treat it as consumed.
        // Fold any such pending wake into this cycle's grant instead.
        let mut grant_reasons = due.reasons.clone();
        let control_at_publication =
            super::control::resident_control_from_history(&history_at_publication)
                .map_err(OrbitError::Execution)?;
        if control_at_publication
            .pending_wake(last_cycle_at_before_publication)
            .is_some()
            && !grant_reasons.iter().any(|reason| reason == "wake")
        {
            grant_reasons.push("wake".to_string());
        }
        if control_at_publication.pending_status_request(last_cycle_at_before_publication)
            && !grant_reasons
                .iter()
                .any(|reason| reason == "status_request")
        {
            grant_reasons.push("status_request".to_string());
        }
        let deadline_passed = mandate.deadline.is_some_and(|deadline| deadline <= now);
        let narrowed_by = if let Some(notice) = due.maintenance.first() {
            Some(format!("notice:{notice}"))
        } else if deadline_passed {
            Some("deadline".to_string())
        } else {
            None
        };
        let grant = ResidentCycleGrant {
            cycle,
            run_id: run_id.to_string(),
            rights: if narrowed_by.is_some() {
                Vec::new()
            } else {
                effective.rights.clone()
            },
            workspaces: effective.workspaces.clone(),
            observe_only: narrowed_by.is_some(),
            narrowed_by,
            recovery_of,
            digest: due.digest.clone(),
            reasons: grant_reasons,
        };
        self.append_resident_history(
            &task_id,
            RESIDENT_CYCLE_GRANTED_EVENT,
            serde_json::to_string(&grant)
                .map_err(|error| OrbitError::Execution(format!("serialize grant: {error}")))?,
        )?;
        // A mandate attached before core owned this status starts here.
        self.activate_resident_assignment(&task_id);
        let history = self.get_task_history(&task_id)?;
        let worktree = self.create_resident_cycle_worktree(run_id);
        self.resident_brief(
            &task_id,
            &mandate,
            &grant,
            &history,
            Some(&due.notices),
            worktree,
        )
    }

    pub(crate) fn crew_supports_continuity(&self, crew_name: &str) -> bool {
        self.resolve_crew_for_task(Some(crew_name), None)
            .is_ok_and(|crew| orbit_engine::provider_supports_continuity(&crew.assignment.provider))
    }

    /// Record that `run_id`'s resident cycle fell back from an expired or
    /// unresumable conversation reference to `continuity: rebuilt`.
    pub fn record_resident_continuity_rebuilt_for_run(
        &self,
        run_id: &str,
    ) -> Result<(), OrbitError> {
        let Ok(task_id) = self.resident_cycle_mandate_of_run(run_id) else {
            return Ok(());
        };
        let history = self.get_task_history(&task_id)?;
        let cycle = live_cycle_from_history(&history)?
            .filter(|live| live.run_id == run_id)
            .map(|live| live.cycle)
            .unwrap_or_else(|| resident_cycle_id(run_id));
        if !super::due::continuity_rebuilt(&history, &cycle) {
            self.append_resident_history(
                &task_id,
                orbit_types::workflow::RESIDENT_CONTINUITY_REBUILT_EVENT,
                cycle,
            )?;
        }
        Ok(())
    }

    /// The brief a cycle's agent reads, in the checkpoint contract's order.
    #[allow(
        clippy::too_many_arguments,
        reason = "the brief is assembled from the mandate, grant, history, notices and launch worktree together"
    )]
    fn resident_brief(
        &self,
        task_id: &str,
        mandate: &orbit_types::workflow::ResidentMandate,
        grant: &ResidentCycleGrant,
        history: &[TaskHistoryEntry],
        notices: Option<&[orbit_types::record::Notice]>,
        worktree: Option<std::path::PathBuf>,
    ) -> Result<Value, OrbitError> {
        let comments = self.get_task_comments(task_id)?;
        let all = checkpoints(task_id, &comments);
        let since = started_cycles(history)
            .iter()
            .rev()
            .find(|started| started.live.cycle != grant.cycle)
            .map(|started| started.at);
        let (continuity, conversation_ref) = match mandate.continuity {
            orbit_types::workflow::ResidentContinuity::Off => ("off", None),
            orbit_types::workflow::ResidentContinuity::Prefer => {
                let resumed_ref = (!super::due::continuity_rebuilt(history, &grant.cycle))
                    .then(|| {
                        all.last()
                            .filter(|last| last.crew == mandate.crew)
                            .and_then(|last| last.conversation_ref.as_deref())
                            .map(str::trim)
                            .filter(|value| !value.is_empty())
                            .filter(|_| self.crew_supports_continuity(&mandate.crew))
                            .map(str::to_string)
                    })
                    .flatten();
                if resumed_ref.is_some() {
                    ("resumed", resumed_ref)
                } else {
                    ("rebuilt", None)
                }
            }
        };
        let recent: Vec<ResidentCheckpoint> = all
            .iter()
            .rev()
            .take(RESIDENT_BRIEF_CHECKPOINTS)
            .cloned()
            .map(|mut checkpoint| {
                checkpoint.conversation_ref = None;
                checkpoint
            })
            .collect();
        let older_checkpoint_notes: Vec<Value> = all
            .iter()
            .rev()
            .skip(RESIDENT_BRIEF_CHECKPOINTS)
            .map(|checkpoint| json!({"cycle": checkpoint.cycle, "notes": checkpoint.notes}))
            .collect();
        let messages: Vec<Value> = comments
            .iter()
            .filter(|comment| since.is_none_or(|since| comment.at >= since))
            .filter(|comment| {
                !matches!(
                    ResidentComment::parse(&comment.message),
                    Ok(ResidentComment::Checkpoint(_) | ResidentComment::Request(_))
                )
            })
            .map(|comment| json!({"at": comment.at, "by": comment.by, "message": comment.message}))
            .collect();
        let recovered = grant
            .recovery_of
            .as_deref()
            .map(|dead| action_rows(history, dead))
            .unwrap_or_default();
        let open_requests = self.resident_open_requests(task_id, mandate, all.last(), &comments)?;
        let resolved_notices;
        let in_scope_notices = match notices {
            Some(notices) => notices,
            None => {
                resolved_notices = self.resident_notices(mandate).unwrap_or_default();
                &resolved_notices
            }
        };
        let notice_entries: Vec<Value> = in_scope_notices
            .iter()
            .map(|notice| {
                json!({
                    "id": notice.id,
                    "kind": notice.kind,
                    "status": notice.status,
                    "workspace": notice.workspace.name,
                    "untrusted_title": notice.title,
                    "untrusted_body": notice.body,
                })
            })
            .collect();
        Ok(json!({
            "task_id": task_id,
            "cycle_count": 1,
            // The managed worktree the cycle's agent launches in, removed
            // by `resident_finalize`. Null when none could be created.
            "workspace_path": worktree.map(|path| path.display().to_string()),
            "cycle": grant.cycle,
            "crew": mandate.crew,
            "continuity": continuity,
            "conversation_ref": conversation_ref,
            "action_key_prefix": format!("resident:{task_id}:{}:", grant.cycle),
            "mandate": mandate,
            "standing_goals": {
                "goal": mandate.goal,
                "exit_criteria": mandate.exit_criteria,
            },
            "authority": grant,
            "checkpoints": recent,
            "older_checkpoint_notes": older_checkpoint_notes,
            "open_requests": open_requests,
            "comments_since_last_cycle": messages,
            "wake": {"reasons": grant.reasons, "digest": grant.digest},
            // A manager asked for status: checkpoint even when idle.
            "status_request": grant.reasons.iter().any(|reason| reason == "status_request"),
            "notices": notice_entries,
            "recovery_of": grant.recovery_of,
            "recovered_actions": recovered,
        }))
    }

    pub(super) fn resident_open_requests(
        &self,
        task_id: &str,
        mandate: &orbit_types::workflow::ResidentMandate,
        last_checkpoint: Option<&ResidentCheckpoint>,
        comments: &[TaskComment],
    ) -> Result<Vec<Value>, OrbitError> {
        let mut resident_requests = Vec::new();
        let mut resident_answers = Vec::new();
        for comment in comments {
            match ResidentComment::parse(&comment.message) {
                Ok(ResidentComment::Request(request)) if request.mandate == task_id => {
                    resident_requests.push(request);
                }
                Ok(ResidentComment::Answer(answer)) if answer.mandate == task_id => {
                    resident_answers.push(answer);
                }
                _ => {}
            }
        }
        let watched = self.resident_watched_tasks(mandate, last_checkpoint)?;
        let mut task_ids = Vec::new();
        let mut seen_tasks = std::collections::BTreeSet::new();
        for id in std::iter::once(task_id.to_string())
            .chain(watched.into_iter().map(|task| task.id.to_string()))
            .chain(
                resident_requests
                    .iter()
                    .map(|request| request.task.clone())
                    .filter(|task| !task.is_empty()),
            )
        {
            if seen_tasks.insert(id.clone()) {
                task_ids.push(id);
            }
        }
        let mut entries = Vec::new();
        let mut matched_decision_ids = std::collections::BTreeSet::new();
        for id in task_ids {
            let Ok(requests) = self.get_task_decision_requests(&id) else {
                continue;
            };
            for request in requests {
                let resident_request = resident_requests
                    .iter()
                    .find(|entry| entry.decision_request == request.id);
                if !matches!(
                    request.state,
                    orbit_types::task::DecisionRequestState::Open
                        | orbit_types::task::DecisionRequestState::Answered
                ) && resident_request.is_none()
                {
                    continue;
                }
                matched_decision_ids.insert(request.id.clone());
                let mut entry = crate::application::task::decision_request_json(&request);
                if let Some(resolution) = &request.resolution {
                    let chosen_label = request
                        .answers
                        .iter()
                        .find(|answer| answer.id == resolution.answer_id)
                        .map(|answer| answer.label.as_str())
                        .unwrap_or(resolution.answer_id.as_str());
                    entry["answer_id"] = json!(resolution.answer_id);
                    entry["answer"] = json!(chosen_label);
                    entry["answered_by"] = json!(resolution.answered_by);
                }
                if let Some(resident_request) = resident_request {
                    entry["request_id"] = json!(resident_request.id);
                    entry["decision_request"] = json!(resident_request.decision_request);
                    entry["class"] = json!(resident_request.class);
                    entry["question"] = json!(resident_request.question);
                    entry["blocking"] = json!(resident_request.blocking);
                    entry["blocks"] = json!(resident_request.blocks);
                }
                let matching_answers: Vec<_> = resident_answers
                    .iter()
                    .filter(|answer| {
                        answer.request_id.as_deref() == Some(request.id.as_str())
                            || resident_request.is_some_and(|req| {
                                answer.request_id.as_deref() == Some(req.id.as_str())
                            })
                    })
                    .collect();
                if !matching_answers.is_empty() {
                    entry["resident_answers"] = json!(matching_answers);
                    if entry.get("answer").is_none()
                        && let Some(latest) = matching_answers.last()
                    {
                        entry["answer"] = json!(latest.quote);
                    }
                }
                entries.push(entry);
            }
        }
        for resident_request in &resident_requests {
            if matched_decision_ids.contains(&resident_request.decision_request) {
                continue;
            }
            let matching_answers: Vec<_> = resident_answers
                .iter()
                .filter(|answer| {
                    answer.request_id.as_deref() == Some(resident_request.id.as_str())
                        || answer.request_id.as_deref()
                            == Some(resident_request.decision_request.as_str())
                })
                .collect();
            entries.push(json!({
                "id": resident_request.id,
                "request_id": resident_request.id,
                "cycle": resident_request.cycle,
                "decision_request": resident_request.decision_request,
                "task": resident_request.task,
                "class": resident_request.class,
                "question": resident_request.question,
                "recommended": resident_request.recommended,
                "blocking": resident_request.blocking,
                "blocks": resident_request.blocks,
                "state": if matching_answers.is_empty() { "open" } else { "answered" },
                "answer": matching_answers.last().map(|answer| answer.quote.as_str()),
                "answers": matching_answers,
            }));
        }
        Ok(entries)
    }

    /// `resident_finalize`: close this run's cycle. A cycle that wrote no
    /// checkpoint gets a synthesized `incomplete` one first. A `handoff`
    /// checkpoint moves the assignment to review, as does a stop while the
    /// cycle ran. A run that holds no live cycle (it skipped, or a newer cycle
    /// replaced it) changes nothing.
    pub fn finish_resident_cycle(&self, run_id: &str) -> Result<Value, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        let task_id = self.resident_cycle_mandate_of_run(run_id)?;
        let history = self.get_task_history(&task_id)?;
        let Some(live) = live_cycle_from_history(&history)?.filter(|live| live.run_id == run_id)
        else {
            return Ok(json!({"task_id": task_id, "finalized": false}));
        };
        let synthesized = self.synthesize_resident_checkpoint(
            &task_id,
            &live,
            "the cycle's agent ended without a valid checkpoint",
            None,
        )?;
        self.set_resident_live_cycle(&task_id, Some(&live.cycle), None)?;
        // The agent handed off, or the mandate was stopped while this cycle ran.
        let handoff = match self.settle_resident_handoff(&task_id)? {
            Some(handoff) => handoff,
            None => self.hand_off_stopped_resident(&task_id)?,
        };
        // The cycle is closed; its managed worktree is discarded.
        self.discard_resident_cycle_worktree(run_id);
        Ok(json!({
            "task_id": task_id,
            "finalized": true,
            "cycle": live.cycle,
            "synthesized": synthesized,
            "handoff": handoff,
        }))
    }

    /// Write the `incomplete` checkpoint for `cycle` unless it already has
    /// one. Trusted: the cycle is no longer live, so this bypasses the
    /// live-cycle admission ordinary checkpoint writes need. Returns whether
    /// it wrote one.
    pub(crate) fn synthesize_resident_checkpoint(
        &self,
        task_id: &str,
        cycle: &ResidentLiveCycle,
        error: &str,
        now: Option<DateTime<Utc>>,
    ) -> Result<bool, OrbitError> {
        let now = now.unwrap_or_else(Utc::now);
        let mut wrote = false;
        self.stores()
            .tasks()
            .with_task_write_lock(task_id, &mut || {
                let comments = self.get_task_comments(task_id)?;
                if checkpoints(task_id, &comments)
                    .iter()
                    .any(|checkpoint| checkpoint.cycle == cycle.cycle)
                {
                    return Ok(());
                }
                let history = self.get_task_history(task_id)?;
                let grant = cycle_grant(&history, &cycle.cycle);
                let mandate = super::resident_mandate_from_artifact(self, task_id)?
                    .map_err(OrbitError::InvalidInput)?;
                let actions = action_rows(&history, &cycle.cycle)
                    .into_iter()
                    .take(50)
                    .map(|row| ResidentCheckpointAction {
                        kind: row.tool,
                        subject: row.subject.unwrap_or_else(|| task_id.to_string()),
                        // A call that started and never reported is in doubt:
                        // recorded failed, and refused if retried.
                        result: row.phase.result().unwrap_or("failed").to_string(),
                        reference: row.reference,
                        key: row.key,
                    })
                    .collect();
                let digest = grant.as_ref().map_or_else(
                    || format!("sha256:{}", "0".repeat(64)),
                    |grant| grant.digest.clone(),
                );
                let record = ResidentCheckpoint {
                    schema: 1,
                    mandate: task_id.to_string(),
                    cycle: cycle.cycle.clone(),
                    run_id: cycle.run_id.clone(),
                    crew: mandate.crew.clone(),
                    recovery_of: grant.as_ref().and_then(|grant| grant.recovery_of.clone()),
                    wake: ResidentCheckpointWake {
                        reasons: grant
                            .as_ref()
                            .map(|grant| grant.reasons.clone())
                            .filter(|reasons| !reasons.is_empty())
                            .unwrap_or_else(|| vec!["unknown".into()]),
                        digest,
                    },
                    authority: ResidentCheckpointAuthority {
                        rights: grant.as_ref().map(|g| g.rights.clone()).unwrap_or_default(),
                        observe_only: grant.as_ref().is_none_or(|g| g.observe_only),
                        narrowed_by: grant.as_ref().and_then(|g| g.narrowed_by.clone()),
                    },
                    actions,
                    decisions: Vec::new(),
                    requests: Vec::new(),
                    costs: Vec::new(),
                    notes: String::new(),
                    next_wake: ResidentNextWake {
                        reason: "recovery".into(),
                        not_before: now,
                    },
                    continuity: "off".into(),
                    outcome: "incomplete".into(),
                    probe: mandate.probe,
                    error: Some(error.to_string()),
                    conversation_ref: None,
                };
                let message = serde_json::to_string(&ResidentComment::Checkpoint(Box::new(record)))
                    .map_err(|error| {
                        OrbitError::Execution(format!("serialize checkpoint: {error}"))
                    })?;
                ResidentComment::parse(&message)
                    .map_err(|error| OrbitError::Execution(error.to_string()))?;
                self.stores().task_records().update(
                    task_id,
                    TaskRecordUpdateParams {
                        actor: "system".into(),
                        append_comments: vec![TaskComment {
                            at: now,
                            by: "system".into(),
                            message,
                            operation_id: None,
                        }],
                        ..Default::default()
                    },
                )?;
                wrote = true;
                Ok(())
            })?;
        Ok(wrote)
    }

    /// Append one system history entry to the assignment.
    pub(crate) fn append_resident_history(
        &self,
        task_id: &str,
        event: &str,
        note: String,
    ) -> Result<(), OrbitError> {
        self.stores().task_records().update(
            task_id,
            TaskRecordUpdateParams {
                actor: "system".into(),
                append_history: vec![TaskHistoryEntry {
                    at: Utc::now(),
                    by: "system".into(),
                    event: event.into(),
                    note: Some(note),
                    from_status: None,
                    to_status: None,
                    operation_id: None,
                    decision: None,
                }],
                ..Default::default()
            },
        )?;
        Ok(())
    }
}
