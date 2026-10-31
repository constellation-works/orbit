//! Capture validation and presentation.

use std::collections::BTreeMap;

use super::capture::*;
use super::review::{VisualReviewRecord, VisualVerdictKind};

/// The one line run show, task show and the PR body print (scope ›
/// Surfaces): what was captured and checked, or why it is not
/// established. Never reads as a pass when nothing was captured.
#[must_use]
pub fn visual_review_summary(
    capture: &VisualCaptureRecord,
    checks: Option<&VisualChecksRecord>,
    settle: Option<&VisualSettleRecord>,
) -> String {
    visual_review_summary_with_review(capture, checks, settle, None)
}

/// [`visual_review_summary`] with the comparison review's outcome
/// appended when one was recorded: findings by verdict, or why the
/// review is not established. Findings are recorded only in shadow.
#[must_use]
pub fn visual_review_summary_with_review(
    capture: &VisualCaptureRecord,
    checks: Option<&VisualChecksRecord>,
    settle: Option<&VisualSettleRecord>,
    review: Option<&VisualReviewRecord>,
) -> String {
    let mode = capture.mode.as_str();
    if let Some(settle) = settle
        && settle.outcome == VisualSettleOutcome::Refused
    {
        let codes = settle
            .refusals
            .iter()
            .map(|refusal| match &refusal.mock {
                Some(mock) => format!("{} ({mock})", refusal.code),
                None => refusal.code.clone(),
            })
            .collect::<Vec<_>>()
            .join(", ");
        return format!("{mode}: capture refused: {codes}");
    }
    if capture.outcome == VisualCaptureOutcome::NotEstablished {
        let reasons = capture
            .not_established
            .iter()
            .map(|reason| reason.reason.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let held = settle.is_some_and(|settle| settle.held);
        return format!(
            "{mode}: not established: {}{}",
            if reasons.is_empty() {
                "unknown"
            } else {
                &reasons
            },
            if held {
                " (held: visual_review.require_established)"
            } else {
                ""
            }
        );
    }
    let captured = capture
        .mocks
        .iter()
        .filter(|mock| mock.capture.is_some())
        .count();
    let failed = capture.mocks.len() - captured;
    let mut line = format!(
        "{mode}: {captured} of {} mock(s) captured",
        capture.mocks.len()
    );
    if failed > 0 {
        line.push_str(&format!(", {failed} without a capture"));
    }
    if !capture.over_budget.is_empty() {
        line.push_str(&format!(", {} over budget", capture.over_budget.len()));
    }
    match checks {
        Some(checks) if checks.findings.is_empty() => line.push_str("; checks: no findings"),
        Some(checks) => {
            let counts = checks
                .severity_counts()
                .into_iter()
                .map(|(severity, count)| format!("{count} {severity}"))
                .collect::<Vec<_>>()
                .join(", ");
            line.push_str(&format!(
                "; checks: {} finding(s) ({counts}){}",
                checks.findings.len(),
                if capture.mode == VisualReviewMode::Live {
                    ""
                } else {
                    ", recorded only"
                }
            ));
        }
        None => line.push_str("; checks: not run"),
    }
    if let Some(review) = review {
        match review.outcome.as_str() {
            "not_established" => {
                let reasons = review
                    .not_established
                    .iter()
                    .map(|reason| reason.reason.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                line.push_str(&format!(
                    "; review: not established: {}",
                    if reasons.is_empty() {
                        "unknown"
                    } else {
                        &reasons
                    }
                ));
            }
            "reviewer_disabled" => line.push_str("; review: disabled"),
            "no_stable_captures" => line.push_str("; review: no stable captures"),
            _ => {
                let (confirmed, rejected, unverified) = review.verdict_counts();
                line.push_str(&format!(
                    "; review: {} finding(s) ({} {}, {} {}, {} {})",
                    review.findings.len(),
                    confirmed,
                    VisualVerdictKind::Confirmed.as_str(),
                    rejected,
                    VisualVerdictKind::Rejected.as_str(),
                    unverified,
                    VisualVerdictKind::Unverified.as_str(),
                ));
                if review.recorded_only {
                    line.push_str(", recorded only");
                } else {
                    let (blocking, optional, recorded) = review.disposition_counts();
                    let unresolved = review.unresolved_blocking().len();
                    line.push_str(&format!(
                        ", {blocking} blocking ({unresolved} unresolved), {optional} optional, \
                         {recorded} recorded"
                    ));
                    let filed = review
                        .findings
                        .iter()
                        .filter(|finding| finding.follow_up.is_some())
                        .count();
                    if filed > 0 {
                        line.push_str(&format!(", {filed} filed as follow-ups"));
                    }
                    if let Some(recapture) = &review.recapture {
                        line.push_str(&format!(", recapture {}", recapture.outcome));
                    }
                }
                if review.invalid_evidence_dropped > 0 {
                    line.push_str(&format!(
                        ", {} dropped as invalid_evidence",
                        review.invalid_evidence_dropped
                    ));
                }
                if !review.budget.not_shown.is_empty() {
                    line.push_str(&format!(
                        ", {} mock(s) not shown",
                        review.budget.not_shown.len()
                    ));
                }
            }
        }
    }
    line
}

/// What the owner holds a claimed leaf's returned capture to (§6.2).
#[derive(Debug, Clone, Copy)]
pub struct VisualCaptureExpectation<'a> {
    pub contract: &'a VisualReviewContract,
    /// Commits a capture may show: the handed-off head, and the head
    /// beneath the reviewer's commit when the review fixed findings.
    pub commits: &'a [String],
    /// This build's Playwright pin.
    pub kit_pin: &'a str,
}

/// Every way `record` (and the image digests `stored_digest` reports for
/// the artifacts it names) disagrees with what the owner captured for the
/// claim. Empty means the returned capture is the contract's.
#[must_use]
pub fn visual_capture_mismatches(
    record: &VisualCaptureRecord,
    expectation: VisualCaptureExpectation<'_>,
    stored_digest: &dyn Fn(&str) -> Option<String>,
) -> Vec<String> {
    let mut mismatches = Vec::new();
    let contract = expectation.contract;
    if record.task_id != contract.task_id {
        mismatches.push(format!(
            "capture names task `{}`, the claim is for `{}`",
            record.task_id, contract.task_id
        ));
    }
    if record.mode != contract.mode {
        mismatches.push(format!(
            "capture ran in mode `{}`, the claim captured `{}`",
            record.mode.as_str(),
            contract.mode.as_str()
        ));
    }
    if !expectation.commits.contains(&record.candidate_sha) {
        mismatches.push(format!(
            "capture is of commit `{}`, not the handed-off candidate",
            record.candidate_sha
        ));
    }
    let expected_manifests: BTreeMap<&str, &str> = contract
        .manifests
        .iter()
        .map(|manifest| {
            (
                manifest.reference.path.as_str(),
                manifest.reference.sha256.as_str(),
            )
        })
        .collect();
    let recorded_manifests: BTreeMap<&str, &str> = record
        .manifests
        .iter()
        .map(|manifest| (manifest.path.as_str(), manifest.sha256.as_str()))
        .collect();
    if expected_manifests != recorded_manifests {
        mismatches
            .push("manifest digests differ from the manifests the claim captured".to_string());
    }
    if record.outcome == VisualCaptureOutcome::NotEstablished {
        if record.not_established.is_empty() {
            mismatches.push("a not-established capture names no reason".to_string());
        }
        if !record.images().is_empty() {
            mismatches.push("a not-established capture names images".to_string());
        }
        return mismatches;
    }
    match &record.kit {
        Some(kit) if kit.playwright == expectation.kit_pin => {}
        Some(kit) => mismatches.push(format!(
            "kit pin `{}` is not this build's `{}`",
            kit.playwright, expectation.kit_pin
        )),
        None => mismatches.push("a captured record names no kit".to_string()),
    }
    let (budgeted, over) = contract.budgeted_mocks();
    let expected_mocks: BTreeMap<&str, &str> = budgeted
        .iter()
        .map(|mock| (mock.artifact.as_str(), mock.sha256.as_str()))
        .collect();
    let recorded_mocks: BTreeMap<&str, &str> = record
        .mocks
        .iter()
        .map(|mock| (mock.artifact.as_str(), mock.sha256.as_str()))
        .collect();
    if expected_mocks != recorded_mocks {
        mismatches.push(
            "mock digests differ from the mocks frozen when the claim was admitted".to_string(),
        );
    }
    if record.over_budget != over {
        mismatches.push("over-budget mocks differ from the claim's budget".to_string());
    }
    for mock in &record.mocks {
        let Some(capture) = &mock.capture else {
            continue;
        };
        if !expectation.commits.contains(&capture.commit) {
            mismatches.push(format!(
                "{} shows commit `{}`, not the handed-off candidate",
                mock.artifact, capture.commit
            ));
        }
    }
    for (path, recorded) in record.images() {
        if !is_visual_artifact(path) {
            mismatches.push(format!(
                "image `{path}` is outside `{VISUAL_ARTIFACT_PREFIX}`"
            ));
            continue;
        }
        match stored_digest(path) {
            Some(stored) if stored == recorded => {}
            Some(_) => mismatches.push(format!("image `{path}` digest differs from capture.json")),
            None => mismatches.push(format!("image `{path}` was not returned")),
        }
    }
    mismatches
}
