//! A task sitting in review because delivery held, or because a completion
//! rule held it. Read-only: derived from the task, its history and the newest
//! delivery run. No forge read, no transition, no resume.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_types::task::{Task, TaskDecision, TaskHistoryEntry, TaskStatus};
use orbit_types::workflow::{
    COMPLETION_HELD_EVENT, CompletionHold, DeliveryRefusal, JobRun, JobRunState,
    current_completion_hold,
};

use crate::OrbitRuntime;

/// Which hold is keeping the task in review.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewHoldKind {
    /// A `[[completion.hold]]` rule is still current.
    CompletionHold,
    /// The newest delivery run ended `held` on a `delivery_*` refusal.
    DeliveryHold,
}

impl ReviewHoldKind {
    /// `waiting_on` value and the drain summary's hold label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CompletionHold => "completion_hold",
            Self::DeliveryHold => "delivery_hold",
        }
    }
}

/// One current review hold an operator should see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewHold {
    /// Delivery refusal or completion rule.
    pub kind: ReviewHoldKind,
    /// The `delivery_*` code, when `kind` is a delivery hold.
    pub code: Option<String>,
    /// The completion-hold rule label, when `kind` is a completion hold.
    pub rule: Option<String>,
    /// The completion-hold field that matched.
    pub field: Option<String>,
    /// The configured value that matched.
    pub pattern: Option<String>,
    /// What the completion rule matched.
    pub matched: Option<String>,
    /// The held delivery run, or the run that recorded the completion hold.
    pub run_id: String,
    /// The task's pull-request number, when it has one.
    pub pr_number: Option<String>,
    /// When the hold became current.
    pub since: DateTime<Utc>,
    /// One line naming the code, or the rule and its match.
    pub reason: String,
    /// The documented next step. Reading it changes nothing.
    pub next_summary: String,
}

impl ReviewHold {
    /// The command the next step names. A completion hold's command approves
    /// the task after the pull request is merged; only a forge-observation
    /// delivery hold resumes its run.
    #[must_use]
    pub fn next_command(&self, workspace_id: &str, task_id: &str) -> String {
        match self.kind {
            ReviewHoldKind::CompletionHold => {
                format!("orbit --workspace '{workspace_id}' task update {task_id} --approve")
            }
            ReviewHoldKind::DeliveryHold
                if self.code.as_deref()
                    == Some(DeliveryRefusal::ForgeObservationUnavailable.error_code()) =>
            {
                format!("orbit job resume {}", self.run_id)
            }
            ReviewHoldKind::DeliveryHold => format!("orbit run show {}", self.run_id),
        }
    }
}

fn delivery_run_is_live(run: &JobRun) -> bool {
    matches!(
        run.state,
        JobRunState::Pending | JobRunState::Running | JobRunState::Retrying
    )
}

/// The newest `delivery_*` refusal on a held run. `base_chase_exhausted` is a
/// hold and is not one of these codes.
fn delivery_refusal_code(run: &JobRun) -> Option<DeliveryRefusal> {
    if run.state != JobRunState::Held {
        return None;
    }
    run.steps.iter().rev().find_map(|step| {
        if step.state == JobRunState::Skipped {
            return None;
        }
        let code = step.error_code.as_deref()?;
        let refusal = DeliveryRefusal::from_error_code(code)?;
        refusal
            .error_code()
            .starts_with("delivery_")
            .then_some(refusal)
    })
}

fn completion_recorded_at(
    history: &[TaskHistoryEntry],
    hold: &CompletionHold,
) -> Option<DateTime<Utc>> {
    history
        .iter()
        .rev()
        .find_map(|entry| match &entry.decision {
            Some(TaskDecision::CompletionHeld(recorded))
                if entry.event == COMPLETION_HELD_EVENT && recorded == hold =>
            {
                Some(entry.at)
            }
            _ => None,
        })
}

impl OrbitRuntime {
    /// The hold keeping `task` in review, if the operator should see one.
    ///
    /// A live newest delivery run yields nothing here, so a review that is
    /// still shipping is not reported as held. A `delivery_*` hold on that
    /// newest run wins over a completion hold. Any other outcome, including
    /// no delivery run, falls through to the current completion hold. A read
    /// of the delivery run is returned as an error so the caller can degrade
    /// rather than treat the task as clear.
    pub fn current_review_hold(
        &self,
        task: &Task,
        history: &[TaskHistoryEntry],
    ) -> Result<Option<ReviewHold>, OrbitError> {
        if task.status != TaskStatus::Review {
            return Ok(None);
        }
        let newest = self.newest_single_task_delivery_run(&task.id)?;
        if newest.as_ref().is_some_and(delivery_run_is_live) {
            return Ok(None);
        }
        if let Some(run) = newest.as_ref()
            && let Some(refusal) = delivery_refusal_code(run)
        {
            let code = refusal.error_code();
            return Ok(Some(ReviewHold {
                kind: ReviewHoldKind::DeliveryHold,
                code: Some(code.to_string()),
                rule: None,
                field: None,
                pattern: None,
                matched: None,
                run_id: run.run_id.clone(),
                pr_number: task.github_pr_number().map(str::to_string),
                since: run.finished_at.unwrap_or(run.recency_at()),
                reason: code.to_string(),
                next_summary: refusal.operator_next_step(&run.run_id),
            }));
        }
        let Some(hold) = current_completion_hold(task.status, history) else {
            return Ok(None);
        };
        Ok(Some(ReviewHold {
            kind: ReviewHoldKind::CompletionHold,
            code: None,
            rule: Some(hold.rule.clone()),
            field: Some(hold.field.as_str().to_string()),
            pattern: Some(hold.pattern.clone()),
            matched: Some(hold.matched.clone()),
            run_id: hold.run_id.clone(),
            pr_number: task.github_pr_number().map(str::to_string),
            since: completion_recorded_at(history, hold).unwrap_or(task.updated_at),
            reason: hold.summary(),
            next_summary: "Merge the pull request and approve the task".to_string(),
        }))
    }
}
