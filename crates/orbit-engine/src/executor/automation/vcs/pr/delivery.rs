//! Fresh delivery evidence for completion-authorized PR runs [ORB-11982].
//!
//! F2026-09-102 records a bundle reaching `done` while its pull request was
//! still open after a base-modification race. The recorded task history shows
//! that `review -> done` transition carried no actor and no authorization note,
//! so the completion activity — which always stamps one — did not write it. The
//! surface that did is not recoverable from the retained evidence. This module
//! therefore hardens what *is* in scope: the automatic path must never be the
//! way an open or foreign pull request becomes `done`.
//!
//! The rule is that completion is permitted by evidence read at completion
//! time, about the exact candidate this run published. A run carries the same
//! `head`, `base`, and `published_head_sha` checkpoints [ORB-11488] already
//! pins for conflict repair; [`DeliveryPin`] turns them into an identity the
//! provider's answer has to match, both before a merge is requested and again
//! before the guarded transition runs. A run without those checkpoints — a
//! resumed or hand-built completion input — enforces only what it carries, so
//! the pins narrow the authorized set and never widen it.

use std::path::Path;

use orbit_common::OrbitError;
use serde_json::{Value, json};

use super::super::super::input::input_string_field;
use super::super::freshness::{branch_freshness_against_ref, commit_sha};
use super::super::git::{base_sync_mode_from_input, resolve_worktree_start_point};

/// The candidate identity a completion-authorized run is allowed to deliver.
///
/// Every field is optional because it mirrors an upstream pipeline checkpoint
/// that a given invocation may not carry. An absent pin is not permission: it
/// only means this run has nothing to compare against for that dimension.
pub(super) struct DeliveryPin {
    head: Option<String>,
    base: Option<String>,
    candidate_sha: Option<String>,
}

/// What a merged pull request proved, recorded on the activity output and in
/// the durable authorization note.
pub(super) struct DeliveryEvidence {
    pr_number: String,
    merged_at: String,
    head_ref: Option<String>,
    base_ref: Option<String>,
    head_sha: Option<String>,
    merge_commit: String,
}

impl DeliveryPin {
    pub(super) fn from_input(input: &Value) -> Self {
        Self {
            head: input_string_field(input, "head"),
            base: input_string_field(input, "base").map(|base| {
                base.strip_prefix("origin/")
                    .unwrap_or(base.as_str())
                    .to_string()
            }),
            candidate_sha: input_string_field(input, "published_head_sha"),
        }
    }

    /// Adopt the rewritten candidate an authorized in-run repair produced.
    ///
    /// The bounded conflict recovery rebases and lease-pushes the *same*
    /// branch, so the published SHA legitimately moves once. Without this the
    /// repaired candidate would look like somebody else's head at merge time.
    pub(super) fn adopt_refreshed_candidate(&mut self, candidate_sha: Option<&str>) {
        if let Some(candidate_sha) = candidate_sha.map(str::trim).filter(|sha| !sha.is_empty()) {
            self.candidate_sha = Some(candidate_sha.to_string());
        }
    }

    /// Refuse a pull request that is no longer the one this run published.
    ///
    /// Applied on every poll, so a branch or base that was repointed while the
    /// run waited never receives a merge request in the first place.
    pub(super) fn ensure_candidate_identity(
        &self,
        status: &Value,
        pr_number: &str,
    ) -> Result<(), OrbitError> {
        self.ensure_matches(status, pr_number, "headRefName", self.head.as_deref())?;
        self.ensure_matches(status, pr_number, "baseRefName", self.base.as_deref())
    }

    /// The gate the guarded `review -> done` transition runs behind.
    ///
    /// A merged state is only delivery when the provider also names the merge
    /// commit and the head it merged is the candidate this run authorized.
    pub(super) fn ensure_delivered(
        &self,
        status: &Value,
        pr_number: &str,
    ) -> Result<DeliveryEvidence, OrbitError> {
        self.ensure_candidate_identity(status, pr_number)?;
        self.ensure_matches(
            status,
            pr_number,
            "headRefOid",
            self.candidate_sha.as_deref(),
        )?;

        let merge_commit = reported(status.pointer("/mergeCommit/oid")).ok_or_else(|| {
            OrbitError::Execution(format!(
                "pr_complete: pull request #{pr_number} reports a merged state without a merge \
                 commit, so this run has no evidence of what landed; the task stays in review"
            ))
        })?;

        Ok(DeliveryEvidence {
            pr_number: pr_number.to_string(),
            merged_at: reported(status.get("mergedAt")).unwrap_or_default(),
            head_ref: reported(status.get("headRefName")),
            base_ref: reported(status.get("baseRefName")),
            head_sha: reported(status.get("headRefOid")),
            merge_commit,
        })
    }

    /// Compare one reported identity field against its pin.
    ///
    /// An unpinned field is not checked. A pinned field the provider does not
    /// report is a refusal rather than a pass, because completion cannot claim
    /// an identity it was unable to read.
    fn ensure_matches(
        &self,
        status: &Value,
        pr_number: &str,
        field: &str,
        pinned: Option<&str>,
    ) -> Result<(), OrbitError> {
        let Some(pinned) = pinned else {
            return Ok(());
        };
        let Some(reported) = reported(status.get(field)) else {
            return Err(OrbitError::Execution(format!(
                "delivery_evidence_stale: pull request #{pr_number} did not report {field}, so \
                 the authorized candidate '{pinned}' cannot be confirmed; the task stays in review"
            )));
        };
        if reported != pinned {
            return Err(OrbitError::Execution(format!(
                "delivery_evidence_stale: pull request #{pr_number} reports {field} '{reported}' \
                 but this run published '{pinned}'; completion delivers only the candidate it \
                 published, so the task stays in review"
            )));
        }
        Ok(())
    }

    /// Explain whether red required checks describe the current candidate/base.
    ///
    /// GitHub runs required checks on a merge of the head into the base as it
    /// stood when the check started. Once the base advances past that point the
    /// red result describes a merge ref nobody is proposing any more — the
    /// exact confusion F2026-09-102 recorded, where rebasing onto the current
    /// base turned the same suite green. The answer is best effort: it needs a
    /// readable local checkout, and a refusal is never withheld because the
    /// diagnosis was unavailable.
    pub(super) fn base_advance_note(&self, input: &Value, workspace_path: &str) -> Option<String> {
        let (candidate_sha, base) = (self.candidate_sha.as_deref()?, self.base.as_deref()?);
        let workspace = Path::new(workspace_path);
        let base_ref =
            resolve_worktree_start_point(workspace, base, base_sync_mode_from_input(input).ok()?)
                .ok()?;
        let base_sha = commit_sha(workspace, &base_ref).ok()?;
        let freshness =
            branch_freshness_against_ref(workspace, candidate_sha, &base_ref, &base_sha).ok()?;

        Some(if freshness.commits_behind == 0 {
            format!(
                " The published candidate {candidate_sha} is current with base '{base}' \
                 ({base_sha}), so this is a failure at the current candidate and base."
            )
        } else {
            format!(
                " The published candidate {candidate_sha} is {} commits behind base '{base}' \
                 ({base_sha}), so any red required check ran against a stale merge ref; refresh \
                 the candidate before judging it.",
                freshness.commits_behind
            )
        })
    }
}

impl DeliveryEvidence {
    pub(super) fn as_json(&self) -> Value {
        json!({
            "pr_number": self.pr_number,
            "merged_at": self.merged_at,
            "head_ref": self.head_ref,
            "base_ref": self.base_ref,
            "head_sha": self.head_sha,
            "merge_commit": self.merge_commit,
        })
    }

    /// The provenance appended to the durable completion note, so a later
    /// reader of task history can tell an evidence-backed automatic completion
    /// apart from any other writer of the same transition.
    pub(super) fn authorization_fragment(&self) -> String {
        format!(
            "delivered by pull request #{} merged as {}",
            self.pr_number, self.merge_commit
        )
    }
}

fn reported(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}
