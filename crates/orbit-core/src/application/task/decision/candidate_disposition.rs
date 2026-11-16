//! Derived block-triage answers for a preserved candidate.
//!
//! Accepting an open finding is recorded outside the task specification, so
//! the same candidate stays resumable. A rewrite that would strand that
//! candidate says so when the request is read and discards it when answered.

use orbit_common::OrbitError;
use orbit_types::task::{
    ACCEPT_FINDINGS_ANSWER_ID, DecisionAction, DecisionAnswer, DecisionGate, DecisionOption,
    DecisionRequest, DecisionRequestState, DecisionRiskTier, Task,
};
use orbit_types::workflow::{FindingDisposition, REVIEW_REPORT_ARTIFACT, ReviewReport};

use crate::OrbitRuntime;

/// Words the offer uses when answering the option discards the candidate.
const CANDIDATE_DISCARDED_PHRASE: &str = "candidate is discarded";

impl OrbitRuntime {
    /// Offer acceptance of open findings, and say when a rewrite discards the
    /// preserved candidate. Stored requests are unchanged.
    pub(in crate::application::task) fn add_candidate_disposition_answers(
        &self,
        task: &Task,
        requests: &mut [DecisionRequest],
    ) -> Result<(), OrbitError> {
        for request in requests {
            if request.gate != DecisionGate::BlockTriage
                || request.state != DecisionRequestState::Open
            {
                continue;
            }
            for answer in &mut request.answers {
                let Some(rewrite) = answer
                    .option
                    .as_ref()
                    .and_then(|option| option.rewrite.as_ref())
                else {
                    continue;
                };
                if self.rewrite_strands_candidate(
                    task,
                    rewrite.description.as_deref(),
                    rewrite.acceptance_criteria.as_deref(),
                )? {
                    disclose_discard(answer);
                }
            }
            if request
                .answers
                .iter()
                .any(|answer| answer.id == ACCEPT_FINDINGS_ANSWER_ID)
            {
                continue;
            }
            if !self.accept_findings_resumable(task)? {
                continue;
            }
            request.answers.push(accept_findings_answer());
        }
        Ok(())
    }

    /// Open findings of the latest review that named any, from its settlement
    /// or, when that settlement named none, from the review report.
    pub(in crate::application::task) fn open_review_finding_lines(
        &self,
        task: &Task,
    ) -> Result<Vec<String>, OrbitError> {
        let history = self.get_task_history(&task.id)?;
        let comments = self.get_task_comments(&task.id)?;
        let settlements = orbit_types::task::recorded_review_settlements(&history, &comments);
        if let Some((_, latest)) = settlements.last()
            && !latest.findings.is_empty()
        {
            return Ok(latest
                .findings
                .iter()
                .filter(|finding| finding.disposition == FindingDisposition::Open)
                .map(|finding| finding.line.clone())
                .collect());
        }
        self.review_report_open_lines(&task.id)
    }

    /// Whether accepting the open findings can resume `task`'s candidate.
    /// A refusal to resume hides the offer; it does not fail the decision read.
    fn accept_findings_resumable(&self, task: &Task) -> Result<bool, OrbitError> {
        if self.unsettled_run(task)?.is_some() || !self.preserved_candidate_bound(task)? {
            return Ok(false);
        }
        if self.open_review_finding_lines(task)?.is_empty() {
            return Ok(false);
        }
        match self.authenticate_published_backlog_return(task) {
            Ok(_) => Ok(true),
            Err(OrbitError::InvalidInput(_)) => Ok(false),
            Err(error) => Err(error),
        }
    }

    fn review_report_open_lines(&self, task_id: &str) -> Result<Vec<String>, OrbitError> {
        let Some(artifact) = self.get_task_artifact(task_id, REVIEW_REPORT_ARTIFACT)? else {
            return Ok(Vec::new());
        };
        let Ok(report) = ReviewReport::parse(&artifact.content) else {
            return Ok(Vec::new());
        };
        Ok(report
            .findings
            .iter()
            .filter(|finding| finding.disposition == FindingDisposition::Open)
            .map(|finding| {
                format!(
                    "`{}` [{}, open] {}",
                    finding.id.trim(),
                    finding.severity.trim(),
                    finding.summary.trim()
                )
            })
            .collect())
    }
}

fn disclose_discard(answer: &mut DecisionAnswer) {
    if !answer.label.contains(CANDIDATE_DISCARDED_PHRASE) {
        answer.label = format!("{} ({CANDIDATE_DISCARDED_PHRASE})", answer.label);
    }
    if !answer.consequence.contains(CANDIDATE_DISCARDED_PHRASE) {
        if !answer.consequence.is_empty() && !answer.consequence.ends_with('.') {
            answer.consequence.push('.');
        }
        answer
            .consequence
            .push_str(" The reviewed candidate is discarded.");
    }
}

fn accept_findings_answer() -> DecisionAnswer {
    let summary = "Accept the open findings and resume the reviewed candidate";
    DecisionAnswer {
        id: ACCEPT_FINDINGS_ANSWER_ID.into(),
        label: summary.into(),
        consequence: "The open findings are recorded as accepted. The description and acceptance criteria stay unchanged, and the same candidate returns to the backlog for review and landing.".into(),
        risk_tier: DecisionRiskTier::Reversible,
        option: Some(DecisionOption {
            summary: summary.into(),
            rewrite: None,
            action: Some(DecisionAction::Requeue),
        }),
    }
}
