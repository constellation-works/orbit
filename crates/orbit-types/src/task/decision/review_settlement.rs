//! Settled review facts. Comments written before typed history use one legacy decoder.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::task::{TaskComment, TaskDecision, TaskHistoryEntry};
use crate::workflow::{FindingDisposition, ReviewCertificate, ReviewVerdict};

/// History event written atomically with the settlement's display comment.
pub const REVIEW_SETTLED_EVENT: &str = "review_settled";

/// The gate that published a review settlement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewSettlementGate {
    /// Review before opening the pull request.
    BeforePr,
    /// Review before landing the pull request.
    BeforeLanding,
}

impl ReviewSettlementGate {
    /// The invocation step whose runtime and effort belong to this gate.
    pub const fn activity_id(self) -> &'static str {
        match self {
            Self::BeforePr => "review",
            Self::BeforeLanding => "landing_review",
        }
    }
}

/// A finding's settled state and paths, with its display line retained verbatim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordedReviewFinding {
    pub severity: String,
    pub disposition: FindingDisposition,
    pub paths: BTreeSet<String>,
    pub line: String,
}

/// Review facts published by the gate, rather than recovered from its prose.
/// Optional fields preserve missing or malformed fields in legacy comments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordedReviewSettlement {
    pub gate: ReviewSettlementGate,
    pub attempt_id: String,
    pub verdict: ReviewVerdict,
    pub reviewer_crew: Option<String>,
    pub reviewer_model: Option<String>,
    pub reviewed_commit: Option<String>,
    pub final_commit: Option<String>,
    pub runtime_seconds: Option<u64>,
    pub findings: Vec<RecordedReviewFinding>,
}

impl RecordedReviewSettlement {
    /// Project an authoritative certificate at publication, without parsing display text.
    pub fn from_certificate(certificate: &ReviewCertificate, gate: ReviewSettlementGate) -> Self {
        Self {
            gate,
            attempt_id: certificate.attempt_id.clone(),
            verdict: certificate.verdict,
            reviewer_crew: Some(certificate.reviewer.crew.clone()),
            reviewer_model: Some(certificate.reviewer.model.clone()),
            reviewed_commit: Some(certificate.reviewed_candidate.commit.clone()),
            final_commit: Some(certificate.final_candidate.commit.clone()),
            runtime_seconds: Some(certificate.consumed.seconds),
            findings: certificate
                .findings
                .iter()
                .map(|finding| {
                    let state = match &finding.disposition {
                        FindingDisposition::Open => "open".to_string(),
                        FindingDisposition::Repaired => "fixed".to_string(),
                        FindingDisposition::Disposed { reason } => {
                            format!("disposed: {}", one_line(reason))
                        }
                    };
                    RecordedReviewFinding {
                        severity: finding.severity.trim().to_ascii_lowercase(),
                        disposition: finding.disposition.clone(),
                        paths: finding.paths.iter().cloned().collect(),
                        line: format!(
                            "`{}` [{}, {}] {}",
                            finding.id,
                            finding.severity,
                            state,
                            one_line(&finding.summary)
                        ),
                    }
                })
                .collect(),
        }
    }

    /// Publication replay identity. Scoring deliberately deduplicates by attempt alone.
    pub fn same_publication(&self, other: &Self) -> bool {
        self.gate == other.gate
            && self.attempt_id == other.attempt_id
            && self.verdict == other.verdict
    }

    /// A history row whose timestamp is the display comment's timestamp.
    pub fn history_entry(&self, at: DateTime<Utc>, by: &str) -> TaskHistoryEntry {
        TaskHistoryEntry::new(at, by, REVIEW_SETTLED_EVENT)
            .with_decision(Some(TaskDecision::ReviewSettlement(self.clone())))
    }
}

/// Read typed history and older comment-only settlements at the task-facts boundary.
/// Typed rows suppress decoding their paired comment. Replayed representations keep
/// the earliest timestamp, preserving invocation/runtime attribution.
pub fn recorded_review_settlements(
    history: &[TaskHistoryEntry],
    comments: &[TaskComment],
) -> Vec<(DateTime<Utc>, RecordedReviewSettlement)> {
    let mut records: Vec<_> = history
        .iter()
        .filter_map(|entry| {
            if let Some(TaskDecision::ReviewSettlement(settlement)) = &entry.decision {
                Some((entry.at, settlement.clone()))
            } else {
                None
            }
        })
        .collect();
    let paired: BTreeSet<_> = history
        .iter()
        .filter(|entry| matches!(entry.decision, Some(TaskDecision::ReviewSettlement(_))))
        .map(|entry| (entry.at, entry.by.as_str()))
        .collect();
    for comment in comments {
        if paired.contains(&(comment.at, comment.by.as_str())) {
            continue;
        }
        if let Some(legacy) = legacy_review_settlement(&comment.message) {
            if let Some((at, recorded)) = records
                .iter_mut()
                .find(|(_, record)| record.same_publication(&legacy))
            {
                if comment.at < *at {
                    *at = comment.at;
                    *recorded = legacy;
                }
            } else {
                records.push((comment.at, legacy));
            }
        }
    }
    records.sort_by_key(|(at, _)| *at);
    records
}

/// The only decoder of legacy settlement prose; absent/malformed optional fields
/// stay absent, while an unrecognized lead or verdict is not a settlement.
fn legacy_review_settlement(message: &str) -> Option<RecordedReviewSettlement> {
    let message = message.trim_start();
    let (gate, rest) =
        if let Some(rest) = message.strip_prefix("before-PR review settled attempt `") {
            (ReviewSettlementGate::BeforePr, rest)
        } else {
            (
                ReviewSettlementGate::BeforeLanding,
                message.strip_prefix("before-landing review settled attempt `")?,
            )
        };
    let (attempt_id, rest) = rest.split_once('`')?;
    let verdict = rest.strip_prefix(": verdict **")?.split_once("**")?.0;
    let verdict = serde_json::from_value(serde_json::Value::from(verdict)).ok()?;
    let mut settlement = RecordedReviewSettlement {
        gate,
        attempt_id: attempt_id.to_string(),
        verdict,
        reviewer_crew: None,
        reviewer_model: None,
        reviewed_commit: None,
        final_commit: None,
        runtime_seconds: None,
        findings: Vec::new(),
    };
    for line in message.lines() {
        if let Some(rest) = line.strip_prefix("- Reviewer: crew `")
            && let Some((crew, rest)) = rest.split_once('`')
        {
            settlement.reviewer_crew = Some(crew.to_string());
            settlement.reviewer_model = rest
                .split_once(" / ")
                .and_then(|(_, model)| model.split_once(')'))
                .map(|(model, _)| model.trim().to_string())
                .filter(|model| !model.is_empty());
        } else if let Some(rest) = line.strip_prefix("- Implementation: `") {
            settlement.reviewed_commit = legacy_commit(rest);
        } else if let Some(rest) = line.strip_prefix("- Final candidate: `") {
            settlement.final_commit = legacy_commit(rest);
        } else if let Some(rest) = line.strip_prefix("- Reviewer runtime: ") {
            settlement.runtime_seconds = rest
                .split_once('s')
                .and_then(|(seconds, _)| seconds.trim().parse().ok());
        } else if let Some(finding) = legacy_finding(line) {
            settlement.findings.push(finding);
        } else if let Some(sub) = line.strip_prefix("  ")
            && let Some(finding) = settlement.findings.last_mut()
        {
            match finding.disposition {
                FindingDisposition::Repaired => {
                    if let Some(change) = sub.strip_prefix("- Changed: ") {
                        finding.paths = change
                            .strip_suffix(')')
                            .and_then(|change| change.rsplit_once(" ("))
                            .map(|(_, paths)| legacy_paths(paths))
                            .unwrap_or_default();
                    }
                }
                FindingDisposition::Open | FindingDisposition::Disposed { .. } => {
                    if let Some(paths) = sub.strip_prefix("- Cited: ") {
                        finding.paths = legacy_paths(paths);
                    }
                }
            }
        }
    }
    Some(settlement)
}

fn legacy_finding(line: &str) -> Option<RecordedReviewFinding> {
    let rest = line.strip_prefix("- `")?;
    let (_, rest) = rest.split_once("` [")?;
    let (severity, rest) = rest.split_once(", ")?;
    let disposition = if rest.starts_with("open]") {
        FindingDisposition::Open
    } else if rest.starts_with("fixed]") {
        FindingDisposition::Repaired
    } else {
        let reason = rest.strip_prefix("disposed: ")?;
        FindingDisposition::Disposed {
            reason: reason
                .split_once(']')
                .map_or(reason, |(reason, _)| reason)
                .to_string(),
        }
    };
    Some(RecordedReviewFinding {
        severity: severity.trim().to_ascii_lowercase(),
        disposition,
        paths: BTreeSet::new(),
        line: line.trim_start_matches("- ").to_string(),
    })
}

fn legacy_commit(rest: &str) -> Option<String> {
    let (commit, _) = rest.split_once('`')?;
    (!commit.is_empty()).then(|| commit.to_string())
}

fn legacy_paths(paths: &str) -> BTreeSet<String> {
    paths
        .split(", ")
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(str::to_string)
        .collect()
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}
