use std::collections::BTreeSet;

use orbit_types::task::TaskComment;
use orbit_types::workflow::ReviewVerdict;
use serde_json::Value;

use super::super::candidates::Candidates;
use super::super::records::bounded;
use super::model::{
    CHANGED_LINE, CITED_LINE, FINAL_CANDIDATE_LINE, Finding, FindingState, IMPLEMENTATION_LINE,
    REVIEWER_LINE, RUNTIME_LINE, RejectOutcome, Review, Runner, SETTLEMENT_LEADS,
};

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

/// A review gate settlement comment: its attempt, verdict, reviewer, the
/// commits it names, its runtime and its findings.
pub(super) fn settlement(comment: &TaskComment) -> Option<Review> {
    let message = comment.message.trim_start();
    let rest = SETTLEMENT_LEADS
        .iter()
        .find_map(|lead| message.strip_prefix(lead))?;
    let (attempt_id, rest) = rest.split_once('`')?;
    let verdict = rest.strip_prefix(": verdict **")?.split_once("**")?.0;
    let verdict: ReviewVerdict = serde_json::from_value(Value::from(verdict)).ok()?;
    let mut review = Review {
        attempt_id: attempt_id.to_string(),
        at: comment.at,
        verdict,
        reviewer: Runner::default(),
        findings: Vec::new(),
        reviewed: None,
        final_candidate: None,
        seconds: None,
        fix_failed: false,
        infrastructure: false,
        outcome: RejectOutcome::Pending,
    };
    for line in message.lines() {
        if let Some(rest) = line.strip_prefix(REVIEWER_LINE)
            && let Some((crew, rest)) = rest.split_once('`')
        {
            review.reviewer.crew = Some(crew.to_string());
            review.reviewer.model = rest
                .split_once(" / ")
                .and_then(|(_, model)| model.split_once(')'))
                .map(|(model, _)| model.trim().to_string())
                .filter(|model| !model.is_empty());
        } else if let Some(rest) = line.strip_prefix(IMPLEMENTATION_LINE) {
            review.reviewed = commit(rest);
        } else if let Some(rest) = line.strip_prefix(FINAL_CANDIDATE_LINE) {
            review.final_candidate = commit(rest);
        } else if let Some(rest) = line.strip_prefix(RUNTIME_LINE) {
            review.seconds = rest
                .split_once('s')
                .and_then(|(seconds, _)| seconds.trim().parse().ok());
        } else if let Some(finding) = finding(line) {
            review.findings.push(finding);
        } else if let Some(sub) = line.strip_prefix("  ")
            && let Some(finding) = review.findings.last_mut()
        {
            match finding.state {
                FindingState::Fixed => {
                    if let Some(change) = sub.strip_prefix(CHANGED_LINE) {
                        finding.paths = changed_paths(change);
                    }
                }
                FindingState::Open | FindingState::Disposed => {
                    if let Some(cited) = sub.strip_prefix(CITED_LINE) {
                        finding.paths = path_list(cited);
                    }
                }
            }
        }
    }
    Some(review)
}

/// A settlement's finding line: ``- `<id>` [<severity>, <state>] <summary>``.
pub(super) fn finding(line: &str) -> Option<Finding> {
    let rest = line.strip_prefix("- `")?;
    let (_, rest) = rest.split_once("` [")?;
    let (severity, rest) = rest.split_once(", ")?;
    let state = if rest.starts_with("open]") {
        FindingState::Open
    } else if rest.starts_with("fixed]") {
        FindingState::Fixed
    } else if rest.starts_with("disposed: ") {
        FindingState::Disposed
    } else {
        return None;
    };
    Some(Finding {
        severity: severity.trim().to_ascii_lowercase(),
        state,
        paths: BTreeSet::new(),
        line: bounded(line.trim_start_matches("- ")),
    })
}

/// The commit a settlement line opens with, between backticks.
pub(super) fn commit(rest: &str) -> Option<String> {
    let (commit, _) = rest.split_once('`')?;
    (!commit.is_empty()).then(|| commit.to_string())
}

/// The paths a fixed finding's `Changed:` line ends on, in parentheses.
pub(super) fn changed_paths(change: &str) -> BTreeSet<String> {
    change
        .strip_suffix(')')
        .and_then(|change| change.rsplit_once(" ("))
        .map(|(_, paths)| path_list(paths))
        .unwrap_or_default()
}

/// A comma-separated path list.
pub(super) fn path_list(paths: &str) -> BTreeSet<String> {
    paths
        .split(", ")
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(str::to_string)
        .collect()
}

/// The gate escalation of reviewer fixes that failed owner revalidation.
pub(super) fn failed_fix(comment: &TaskComment) -> bool {
    comment.message.contains("## Review gate escalation")
        && comment.message.contains("Verdict: `reject`")
}
