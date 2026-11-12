use std::collections::BTreeSet;

use orbit_types::task::{self, TaskComment, TaskHistoryEntry};
use orbit_types::workflow::{FindingDisposition, ReviewVerdict};

use super::super::candidates::Candidates;
use super::super::records::bounded;
use super::model::{Finding, FindingState, RejectOutcome, Review, Runner};

/// Judge each `reject` by the review that settled after it: which of its
/// open findings' cited paths the next implementation changed, and whether
/// that next review passed.
pub(super) fn judge_rejects(reviews: &mut [Review], candidates: &Candidates) {
    for index in 0..reviews.len() {
        if reviews[index].verdict != ReviewVerdict::Reject {
            continue;
        }
        let review = &reviews[index];
        let cited: BTreeSet<String> = review
            .open()
            .flat_map(|finding| finding.paths.iter().cloned())
            .collect();
        let outcome = match reviews.get(index + 1) {
            None => RejectOutcome::Pending,
            Some(_) if cited.is_empty() => RejectOutcome::Unknown,
            Some(next) => {
                let from = review.final_candidate.as_ref().or(review.reviewed.as_ref());
                match from
                    .zip(next.reviewed.as_ref())
                    .and_then(|(from, to)| candidates.changed(from, to, &cited))
                {
                    Some(changed) => RejectOutcome::Judged {
                        changed,
                        accepted: next.verdict.passed(),
                    },
                    None => RejectOutcome::Unknown,
                }
            }
        };
        reviews[index].outcome = outcome;
    }
}

/// The verdicts of a task's review settlements in order, one per attempt.
pub(crate) fn review_gate_verdicts(
    history: &[TaskHistoryEntry],
    comments: &[TaskComment],
) -> Vec<ReviewVerdict> {
    review_settlement_facts(history, comments)
        .into_iter()
        .map(|review| review.verdict)
        .collect()
}

/// Typed facts, with historical comments decoded once by the shared boundary.
pub(in crate::application::agents) fn review_settlement_facts(
    history: &[TaskHistoryEntry],
    comments: &[TaskComment],
) -> Vec<Review> {
    let mut seen = BTreeSet::new();
    task::recorded_review_settlements(history, comments)
        .into_iter()
        .filter(|(_, settlement)| seen.insert(settlement.attempt_id.clone()))
        .map(|(at, settlement)| Review {
            attempt_id: settlement.attempt_id,
            activity_id: Some(settlement.gate.activity_id().to_string()),
            at,
            verdict: settlement.verdict,
            reviewer: Runner {
                crew: settlement.reviewer_crew,
                model: settlement.reviewer_model,
                ..Default::default()
            },
            findings: settlement
                .findings
                .into_iter()
                .map(|finding| Finding {
                    severity: finding.severity,
                    state: match finding.disposition {
                        FindingDisposition::Open => FindingState::Open,
                        FindingDisposition::Repaired => FindingState::Fixed,
                        FindingDisposition::Disposed { .. } => FindingState::Disposed,
                    },
                    paths: finding.paths,
                    line: bounded(&finding.line),
                })
                .collect(),
            reviewed: settlement.reviewed_commit,
            final_candidate: settlement.final_commit,
            seconds: settlement.runtime_seconds,
            fix_failed: false,
            infrastructure: false,
            outcome: RejectOutcome::Pending,
        })
        .collect()
}

/// The gate escalation of reviewer fixes that failed owner revalidation.
pub(super) fn failed_fix(comment: &TaskComment) -> bool {
    comment.message.contains("## Review gate escalation")
        && comment.message.contains("Verdict: `reject`")
        // A settlement's finding text can quote escalation prose. The old
        // comment loop classified settlements first, so retain that priority.
        && task::recorded_review_settlements(&[], std::slice::from_ref(comment)).is_empty()
}
