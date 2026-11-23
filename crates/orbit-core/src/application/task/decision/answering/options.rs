//! One-click option answers: proposal admissibility, triage edits and re-pilot.

use orbit_common::{DecisionRefusalCode, OrbitError};
use orbit_types::task::{
    ACCEPT_FINDINGS_ANSWER_ID, DecisionAction, DecisionAnswer, DecisionGate, DecisionOption,
    DecisionRequest, ReadinessGap, ReadinessStage, TaskComment, TaskHistoryEntry, TaskRelation,
    TaskRelationType, TaskStatus, readiness_gaps,
};

use super::{CANDIDATE_DISCARDED_NOTE, DecisionAnswerEffects, TriageEdit, decision_comment};
use crate::OrbitRuntime;
use crate::application::task::TaskRecordUpdateParams;
use crate::application::task::params::{TaskAddParams, TaskUpdateParams};
use crate::application::task::{
    PilotAdmissionHold, TaskJobPort, TaskRecords, TriageTarget, pilot_assessable_status,
};

/// Whether a block-triage option returns the blocked task to `backlog`.
/// Reject, land and archive take another status and do not resume a
/// published candidate.
fn option_returns_blocked_task_to_backlog(action: &Option<DecisionAction>) -> bool {
    !matches!(
        action,
        Some(DecisionAction::Reject | DecisionAction::Land | DecisionAction::Archive)
    )
}

impl OrbitRuntime {
    /// Apply a one-click option as the human: its rewrite, then its action,
    /// then the move out of `blocked` (or the approval or re-pilot of a held
    /// proposed task), with one comment naming the option and the note. The
    /// caller holds the task's write lock and records the resolution (a
    /// blocked task's reject writes `resolution_history` with the status, and
    /// its pull-request closure is returned as deferred effects), so the
    /// whole answer closes the request in one operation. Everything refusable
    /// is refused before the first write, including a source-repair refusal
    /// of the backlog return. A `split` files its follow-up only after that
    /// authentication, so a refusal applies nothing.
    pub(super) fn apply_option_answer(
        &self,
        request: &DecisionRequest,
        answer: &DecisionAnswer,
        option: &DecisionOption,
        (note, actor_label, via): (Option<&str>, &str, &str),
        resolution_history: Vec<TaskHistoryEntry>,
    ) -> Result<DecisionAnswerEffects, OrbitError> {
        let task_id = request.task_id.as_str();
        let task = self.get_task(task_id)?;
        let refuse = |reason: &str| {
            OrbitError::decision_refused(
                DecisionRefusalCode::AnswerNotApplicable,
                format!(
                    "option `{}` cannot be applied ({reason}); nothing was applied and the request stays open",
                    answer.id
                ),
            )
        };
        let blocked = request.gate == DecisionGate::BlockTriage;
        let expected = if blocked {
            TaskStatus::Blocked
        } else {
            TaskStatus::Proposed
        };
        if task.status != expected {
            return Err(refuse(&format!(
                "the task is {}, not {expected}",
                task.status
            )));
        }

        if matches!(option.action, Some(DecisionAction::RePilot)) {
            self.decision_pilot_preflight(task_id)
                .map_err(|error| refuse(&error))?;
        }
        let mut params = TaskUpdateParams::default();
        let mut applied = Vec::new();
        if let Some(rewrite) = &option.rewrite {
            if let Some(description) = &rewrite.description
                && description.trim() != task.description.trim()
            {
                params.description = Some(description.clone());
                applied.push("description rewritten");
            }
            if let Some(criteria) = &rewrite.acceptance_criteria
                && *criteria != task.acceptance_criteria
            {
                params.acceptance_criteria = Some(criteria.clone());
                applied.push("acceptance criteria rewritten");
            }
        }
        let rewrites = !applied.is_empty();
        let action_kind = option.action.as_ref().map(DecisionAction::kind);
        if option.action.is_none() && !rewrites {
            return Err(refuse("its rewrite leaves the task as it is"));
        }
        // A proposed task that the option approves must clear the same
        // screen an approval answer clears, before its rewrite lands.
        let approves = !blocked
            && matches!(
                option.action,
                Some(DecisionAction::Requeue | DecisionAction::ChangeCrew { .. })
                    | Some(DecisionAction::Split { .. })
            );
        if approves {
            self.ensure_proposal_admissible(task_id)?;
        }

        // A rewrite that changes the specification a published or kept
        // candidate implements would be refused for that candidate; the
        // answer discards it with the rewrite instead, and says so.
        let discards = blocked
            && rewrites
            && self.rewrite_strands_candidate(
                &task,
                params.description.as_deref(),
                params.acceptance_criteria.as_deref(),
            )?;
        params.discard_candidate = discards;
        if answer.id == ACCEPT_FINDINGS_ANSWER_ID {
            if rewrites || !matches!(option.action, Some(DecisionAction::Requeue)) {
                return Err(refuse(
                    "accepting an open finding only resumes the reviewed candidate",
                ));
            }
            let findings = self.open_review_finding_lines(&task)?;
            if findings.is_empty() {
                return Err(refuse("no open review finding remains to accept"));
            }
            params.finding_acceptance = Some(format!(
                "accepted open review findings without changing the description or acceptance criteria: {}",
                findings.join("; ")
            ));
        }
        // Source repair of the backlog return is refused before a split
        // files its follow-up. `add_task` commits on its own, so a later
        // refusal of the parent update would leave the follow-up behind.
        // A rewrite that strands the candidate discards it and skips this
        // authentication, the same way the update does. A live run is
        // authenticated on the unchanged task: a failure handoff checks the
        // specification before liveness, so a rewritten preview would refuse
        // the answer for a change it made itself.
        if blocked && !discards && option_returns_blocked_task_to_backlog(&option.action) {
            let mut preview = task.clone();
            if self.unsettled_run(&task)?.is_none() {
                if let Some(description) = &params.description {
                    preview.description.clone_from(description);
                }
                if let Some(criteria) = &params.acceptance_criteria {
                    preview.acceptance_criteria.clone_from(criteria);
                }
            }
            self.authenticate_published_backlog_return(&preview)?;
        }

        let mut consequence = format!("Consequence: {}", answer.consequence);
        if !applied.is_empty() {
            consequence.push_str(&format!("\nApplied: {}.", applied.join(", ")));
        }
        if discards {
            consequence.push_str(CANDIDATE_DISCARDED_NOTE);
        }
        if let Some(kind) = action_kind {
            consequence.push_str(&format!("\nAction: {kind}."));
        }
        let comment = decision_comment(
            request,
            &answer.id,
            &format!("option {} — {}", answer.id, option.summary),
            via,
            note,
            &consequence,
        );
        params.comment = Some(comment.clone());
        let actor = actor_label.to_string();
        let backlog = if blocked {
            Some(TaskStatus::Backlog)
        } else {
            None
        };

        match &option.action {
            None | Some(DecisionAction::Requeue | DecisionAction::RePilot) => {
                params.status = backlog;
            }
            Some(DecisionAction::ChangeCrew { crew }) => {
                params.crew = Some(Some(crew.clone()));
                params.status = backlog;
            }
            Some(DecisionAction::Split {
                title,
                description,
                acceptance_criteria,
            }) => {
                let follow_up = self.add_task(TaskAddParams {
                    title: title.clone(),
                    description: description.clone(),
                    acceptance_criteria: acceptance_criteria.clone(),
                    relations: vec![TaskRelation {
                        relation_type: TaskRelationType::SpawnedFrom,
                        target: task.id.clone(),
                        regression: None,
                    }],
                    context_files: task.context_files.clone(),
                    priority: task.priority,
                    status: Some(TaskStatus::Proposed),
                    comment: Some(format!(
                        "Filed by decision request {} on {} (option {}: {}).",
                        request.id, task.id, answer.id, option.summary
                    )),
                    ..TaskAddParams::default()
                })?;
                params.comment = Some(format!(
                    "{comment}\nFollow-up task filed: {} (proposed).",
                    follow_up.id
                ));
                params.status = backlog;
            }
            Some(DecisionAction::Reject) => {
                let reason = note.map(str::to_string).unwrap_or_else(|| {
                    format!(
                        "rejected via option {} of decision request {}",
                        answer.id, request.id
                    )
                });
                if blocked {
                    let effects = self.reject_blocked_task_for_triage_decision(
                        task_id,
                        reason,
                        Some(comment),
                        resolution_history,
                    )?;
                    return Ok(DecisionAnswerEffects {
                        deferred_approval: Some(effects),
                        ..Default::default()
                    });
                }
                self.reject_task(task_id, reason, Some(comment))?;
                return Ok(DecisionAnswerEffects::default());
            }
            Some(DecisionAction::Land) => {
                return Err(refuse("landing requires the operator handoff route"));
            }
            Some(DecisionAction::Archive) => {
                params.status = Some(TaskStatus::Archived);
            }
        }
        self.update_task_as_human(task_id, params, actor)?;
        let explicit_pilot = matches!(option.action, Some(DecisionAction::RePilot));
        if explicit_pilot {
            self.request_explicit_task_pilot(
                task_id,
                Some(actor_label),
                orbit_types::workflow::JobRunTrigger::cli(),
            )
            .map_err(|error| OrbitError::Execution(format!("The decision option's task edits were saved, but its pilot request failed and the decision resolution was not recorded. Inspect the task and decision before requesting another pilot: {error}")))?;
        }
        let deferred_approval = if approves {
            let (_, effects) = self.approve_task_guarded_deferring_effects(
                task_id,
                note.map(str::to_string),
                None,
            )?;
            Some(effects)
        } else {
            None
        };
        Ok(DecisionAnswerEffects {
            deferred_approval,
            requirements_rewritten: rewrites && !explicit_pilot,
        })
    }

    /// Under the answer's task lock, request the assessment of the edited
    /// requirements. Record the answer first so a re-check sees the choice
    /// in its inputs, and preserve it if job submission fails.
    pub(super) fn request_pilot_after_decision_rewrite(&self, task_id: &str, actor_label: &str) {
        let request_pilot = || -> Result<(), OrbitError> {
            let task = self.get_task(task_id)?;
            if pilot_assessable_status(task.status)
                && !self.is_human_crew(task.crew.as_deref())
                && matches!(
                    self.pilot_admission_hold(task_id)?,
                    Some(PilotAdmissionHold::StaleAssessment(_))
                )
            {
                self.request_explicit_task_pilot(
                    task_id,
                    Some(actor_label),
                    orbit_types::workflow::JobRunTrigger::cli(),
                )
                .map_err(OrbitError::Execution)?;
            }
            Ok(())
        };
        if let Err(error) = request_pilot() {
            let message = format!(
                "The decision answer was saved, but Orbit could not request a fresh pilot. {error}. An operator can run `orbit task repilot {task_id}` on the owner once the task is eligible."
            );
            if let Err(record_error) = self.stores().task_records().update(
                task_id,
                TaskRecordUpdateParams {
                    actor: "system".into(),
                    append_comments: vec![TaskComment {
                        at: chrono::Utc::now(),
                        by: "system".into(),
                        message: message.clone(),
                        operation_id: None,
                    }],
                    ..Default::default()
                },
            ) {
                tracing::warn!(task_id, %message, %record_error, "could not record the pilot submission failure");
            }
        }
    }

    /// The task-pilot preparation and admission checks a digest-raised
    /// proposal approval runs before promotion, unlike the advisory
    /// screened approval an operator surface runs beside its warnings.
    ///
    /// Refuses, before anything is written, while a pilot admission
    /// hold or a `verified_no_diff` finding stands, or while a blocking
    /// readiness gap stands at the proposed stage: no declared footprint
    /// without the no-diff disposition, or an unassessed complexity.
    /// The task stays proposed and the request stays open, so a later
    /// answer can still land once the pilot prepares the task or a
    /// verdict resolves the hold.
    pub(super) fn ensure_proposal_admissible(&self, task_id: &str) -> Result<(), OrbitError> {
        let task = self.get_task(task_id)?;
        if task.status != TaskStatus::Proposed {
            // The approval path itself reports the wrong status.
            return Ok(());
        }
        if let Some(hold) = self.pilot_admission_hold(task_id)? {
            return Err(OrbitError::decision_refused(
                DecisionRefusalCode::AnswerNotApplicable,
                format!(
                    "task-pilot holds this proposal ({}) and an approval does not resolve that hold; nothing was applied, the task stays proposed and the request stays open",
                    pilot_hold_classification(&hold)
                ),
            ));
        }
        if self
            .verified_no_diff(task_id, task.status, &task.tags)?
            .is_some()
        {
            return Err(OrbitError::decision_refused(
                DecisionRefusalCode::AnswerNotApplicable,
                "task-pilot verified this proposal needs no change (pilot_verified_no_diff), so it is held for that disposition instead of promotion; nothing was applied, the task stays proposed and the request stays open",
            ));
        }
        let gaps: Vec<ReadinessGap> = readiness_gaps(
            ReadinessStage::Proposed,
            &task.tags,
            &task.context_files,
            task.complexity,
        )
        .into_iter()
        .filter(ReadinessGap::is_blocking)
        .collect();
        if !gaps.is_empty() {
            let detail = gaps
                .iter()
                .map(|gap| format!("{} (fix: {})", gap.code.as_str(), gap.fix))
                .collect::<Vec<_>>()
                .join("; ");
            return Err(OrbitError::decision_refused(
                DecisionRefusalCode::AnswerNotApplicable,
                format!(
                    "this proposal is not prepared for the backlog ({detail}); nothing was applied, the task stays proposed and the request stays open"
                ),
            ));
        }
        Ok(())
    }

    /// The edit the chosen option of the triage proposal on `task_id`
    /// makes, read back from the proposal comment that raised the request
    /// (the request record carries only answer ids, labels and
    /// consequences). Refuses, before anything is written, an option whose
    /// target or replacement cannot be determined or that would leave the
    /// task as it is: returning such work to backlog would re-admit it with
    /// its blocker unresolved.
    pub(super) fn triage_option_edit(
        &self,
        task_id: &str,
        answer_id: &str,
    ) -> Result<TriageEdit, OrbitError> {
        let refuse = |reason: &str| {
            OrbitError::decision_refused(
                DecisionRefusalCode::AnswerNotApplicable,
                format!(
                    "option `{answer_id}` cannot be applied ({reason}); nothing was applied and the task stays blocked"
                ),
            )
        };
        let option = answer_id
            .parse::<usize>()
            .ok()
            .filter(|option| *option > 0)
            .ok_or_else(|| refuse("it is not an option number"))?;
        let comments = self.get_task_comments(task_id)?;
        let proposal = comments
            .iter()
            .rev()
            .find(|comment| {
                comment
                    .message
                    .starts_with("blocked_task_triage disposition=decision")
            })
            .ok_or_else(|| refuse("the proposal comment is gone"))?;
        let marker = format!("\n\nOption {option}: ");
        let section = proposal
            .message
            .find(&marker)
            .map(|start| &proposal.message[start + marker.len()..])
            .ok_or_else(|| refuse("the proposal comment does not list it"))?;
        let (head, replacement) = section
            .split_once("\nReplacement text:\n")
            .ok_or_else(|| refuse("the proposal comment records no replacement text"))?;
        let target = head
            .lines()
            .last()
            .and_then(|line| line.strip_prefix("Applies to: "))
            .and_then(TriageTarget::parse)
            .ok_or_else(|| refuse("the proposal names no field its replacement rewrites"))?;
        let end = [
            "\n\nOption ",
            "\n\nRecommended:",
            "\nNothing else on this task",
        ]
        .iter()
        .filter_map(|needle| replacement.find(needle))
        .min()
        .unwrap_or(replacement.len());
        let replacement = replacement[..end].trim();
        if replacement.is_empty() {
            return Err(refuse("its replacement text is empty"));
        }
        if replacement.contains("[REDACTED") {
            return Err(refuse("its replacement text was redacted"));
        }
        let task = self.get_task(task_id)?;
        let criteria: Vec<String> = replacement
            .lines()
            .map(|line| {
                let line = line.trim();
                line.strip_prefix("- ")
                    .or_else(|| line.strip_prefix("* "))
                    .unwrap_or(line)
                    .trim()
                    .to_string()
            })
            .filter(|line| !line.is_empty())
            .collect();
        let unchanged = match target {
            TriageTarget::AcceptanceCriteria => criteria == task.acceptance_criteria,
            TriageTarget::Description => replacement == task.description.trim(),
        };
        if unchanged {
            return Err(refuse(&format!(
                "its replacement leaves the task's {} unchanged",
                target.as_str()
            )));
        }
        Ok(TriageEdit {
            target,
            replacement: replacement.to_string(),
            criteria,
        })
    }
}

/// The hold classification a refusal names, in the vocabulary drain
/// approval and admission already report.
fn pilot_hold_classification(hold: &PilotAdmissionHold) -> &'static str {
    match hold {
        PilotAdmissionHold::StaleAssessment(_) => "pilot_assessment_stale",
        PilotAdmissionHold::Duplicate => "pilot_duplicate",
        PilotAdmissionHold::AlreadyLanded => "pilot_already_landed",
        PilotAdmissionHold::OperatorValidation(_) => "operator_validation",
        PilotAdmissionHold::HostOperational(_) => "host_operational",
        PilotAdmissionHold::NativeOs(_) => "native_os",
    }
}
