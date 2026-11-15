//! Retention and explicit read-only drops of review finding evidence.
use super::super::CONTEXT_EVIDENCE_DROPPED;
use super::super::input::required_string;
use super::super::source::{GitPathKind, SourceSnapshot};
use super::selectors;
use orbit_common::fs::selector::{
    anchor_path, canonical_selector, canonical_selector_in_workspace, exists_in_workspace,
};
use orbit_engine::DispatchError;
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::Path;

/// Tags the review templates stamp on the findings they file. Each template
/// requires a finding to carry its evidence — every cited source path and
/// the regression-test location — as `context_files`.
const REVIEW_FINDING_TAGS: [&str; 5] = [
    "code-review",
    "delivery-code-review",
    "qa-full-sweep",
    "qa-sweep",
    "security-review",
];

/// Whether a review filed the task, so omitted evidence needs an explicit
/// inspection reason before the pilot can drop it.
pub(in super::super) fn review_filed(tags: &[String]) -> bool {
    tags.iter()
        .any(|tag| REVIEW_FINDING_TAGS.contains(&tag.as_str()))
}

/// The selectors of a review-filed task's prepared scope that `after` omits
/// and that still resolve, with their recorded kind, at the pinned source (or
/// in the workspace without one). These are the only selectors eligible for
/// a verified read-only drop; unexplained omissions stay in the lock footprint.
/// A selector that no longer resolves is not kept: it falls to
/// the same missing-target handling as any other dropped selector.
pub(in super::super) fn retained_review_evidence(
    action: &str,
    before: &[String],
    after: &[String],
    workspace_root: &Path,
    source: Option<&SourceSnapshot>,
) -> Result<Vec<String>, DispatchError> {
    let mut retained = Vec::new();
    for selector in before {
        if after.contains(selector) || retained.contains(selector) {
            continue;
        }
        let Ok(anchor) = anchor_path(selector) else {
            continue;
        };
        let expected_dir = selector.starts_with("dir:");
        let resolves = match source {
            Some(source) => {
                canonical_selector(selector).ok().as_ref() == Some(selector)
                    && matches!(
                        (
                            source.path_kind(action, workspace_root, &anchor)?,
                            expected_dir
                        ),
                        (GitPathKind::Tree, true) | (GitPathKind::Blob, false)
                    )
            }
            None => {
                canonical_selector_in_workspace(selector, workspace_root)
                    .ok()
                    .as_ref()
                    == Some(selector)
                    && exists_in_workspace(selector, workspace_root)
                    && selectors::validate_selector_target_kind(
                        action,
                        "",
                        selector,
                        workspace_root,
                    )
                    .is_ok()
            }
        };
        if resolves {
            retained.push(selector.clone());
        }
    }
    Ok(retained)
}

/// Validate explicit read-only drops against omitted, resolving review
/// evidence. Proposed targets (including retained creation grants) are never
/// eligible. The agent verifies why a selector needs no substantive change;
/// apply validates the record, and the assessment audit preserves its reason.
pub(in super::super) fn reviewed_evidence_drops(
    action: &str,
    assessment: &Value,
    eligible: &[String],
) -> Result<BTreeSet<String>, DispatchError> {
    let mut dropped = BTreeSet::new();
    let Some(value) = assessment.get(CONTEXT_EVIDENCE_DROPPED) else {
        return Ok(dropped);
    };
    let records = value.as_array().ok_or_else(|| {
        DispatchError::action_failed(
            action,
            format!("`{CONTEXT_EVIDENCE_DROPPED}` must be an array"),
        )
    })?;
    for record in records {
        let selector = required_string(record, "selector", action)?;
        required_string(record, "reason", action)?;
        if record["selector"].as_str() != Some(selector)
            || !eligible.iter().any(|candidate| candidate == selector)
        {
            return Err(DispatchError::action_failed(
                action,
                format!(
                    "`{CONTEXT_EVIDENCE_DROPPED}` selector {selector:?} must exactly name an omitted, \
                     resolving review evidence selector, never a proposed change target or creation grant"
                ),
            ));
        }
        if !dropped.insert(selector.to_string()) {
            return Err(DispatchError::action_failed(
                action,
                format!("`{CONTEXT_EVIDENCE_DROPPED}` repeats selector {selector:?}"),
            ));
        }
    }
    Ok(dropped)
}
