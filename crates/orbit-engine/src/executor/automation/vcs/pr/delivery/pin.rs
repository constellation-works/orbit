use super::super::answers::PullRequestStatus;
use crate::executor::automation::input::input_string_field;
use crate::executor::automation::vcs::freshness::{branch_freshness_against_ref, commit_sha};
use crate::executor::automation::vcs::git::{
    base_sync_mode_from_input, resolve_worktree_start_point,
};
use orbit_common::OrbitError;
use orbit_types::workflow::DeliveryRefusal;
use serde_json::{Value, json};
use std::path::Path;

/// The candidate identity a completion-authorized run is allowed to deliver.
///
/// Every field is optional because it mirrors an upstream pipeline checkpoint
/// that a given invocation may not carry. An absent pin is not permission: it
/// only means this run has nothing to compare against for that dimension.
pub(in crate::executor::automation::vcs) struct DeliveryPin {
    head: Option<String>,
    base: Option<String>,
    candidate_sha: Option<String>,
    previous_candidate_sha: Option<String>,
}

/// What a merged pull request proved, recorded on the activity output and in
/// the durable authorization note.
pub(in crate::executor::automation::vcs) struct DeliveryEvidence {
    pr_number: String,
    merged_at: String,
    head_ref: Option<String>,
    base_ref: Option<String>,
    head_sha: Option<String>,
    merge_commit: String,
}

impl DeliveryPin {
    /// Pin an identity the caller already holds exactly, rather than one read
    /// out of pipeline run input. The owner landing consumer has no run
    /// checkpoints: its pins come from the accepted handoff candidate
    /// [ORB-12499], so every dimension is always pinned.
    pub(in crate::executor::automation::vcs) fn pinned(
        head: &str,
        base: &str,
        candidate_sha: &str,
    ) -> Self {
        Self {
            head: Some(head.to_string()),
            base: Some(base.strip_prefix("origin/").unwrap_or(base).to_string()),
            candidate_sha: Some(candidate_sha.to_string()),
            previous_candidate_sha: None,
        }
    }

    pub(in crate::executor::automation::vcs) fn from_input(input: &Value) -> Self {
        Self {
            head: input_string_field(input, "head"),
            base: input_string_field(input, "base").map(|base| {
                base.strip_prefix("origin/")
                    .unwrap_or(base.as_str())
                    .to_string()
            }),
            candidate_sha: input_string_field(input, "published_head_sha"),
            previous_candidate_sha: input_string_field(input, "previous_published_head_sha"),
        }
    }

    /// The head commit this run is authorized to deliver, after any in-run repair.
    pub(in crate::executor::automation::vcs) fn candidate_sha(&self) -> Option<&str> {
        self.candidate_sha.as_deref()
    }

    /// The task branch this run published, if recorded.
    pub(in crate::executor::automation::vcs) fn head(&self) -> Option<&str> {
        self.head.as_deref()
    }

    /// The exact head replaced by this run's verified push, if recorded.
    pub(in crate::executor::automation::vcs::pr) fn previous_candidate_sha(&self) -> Option<&str> {
        self.previous_candidate_sha.as_deref()
    }

    /// Adopt the rewritten candidate an authorized in-run repair produced.
    ///
    /// The bounded conflict recovery rebases and lease-pushes the *same*
    /// branch, so the published SHA legitimately moves once. Without this the
    /// repaired candidate would look like somebody else's head at merge time.
    pub(in crate::executor::automation::vcs) fn adopt_refreshed_candidate(
        &mut self,
        candidate_sha: Option<&str>,
    ) {
        if let Some(candidate_sha) = candidate_sha.map(str::trim).filter(|sha| !sha.is_empty()) {
            self.previous_candidate_sha = self.candidate_sha.take();
            self.candidate_sha = Some(candidate_sha.to_string());
        }
    }

    /// Refuse a pull request that is no longer the one this run published.
    ///
    /// Applied on every poll, so a branch or base that was repointed while the
    /// run waited never receives a merge request in the first place.
    pub(in crate::executor::automation::vcs) fn ensure_candidate_identity(
        &self,
        status: &PullRequestStatus,
        pr_number: &str,
    ) -> Result<(), OrbitError> {
        ensure_matches(
            pr_number,
            "headRefName",
            status.head_branch.as_deref(),
            self.head.as_deref(),
        )?;
        ensure_matches(
            pr_number,
            "baseRefName",
            status.base_branch.as_deref(),
            self.base.as_deref(),
        )
    }

    /// The full pinned identity, including the head commit.
    ///
    /// Checked before a merge or auto-merge is requested, so a head this run
    /// did not publish never reaches the provider. Completion also sends this
    /// SHA to the synchronous provider mutation, which refuses a head that
    /// moves after the read. The owner landing consumer checks the same
    /// identity on every poll [ORB-12499].
    pub(in crate::executor::automation::vcs) fn ensure_pinned_candidate(
        &self,
        status: &PullRequestStatus,
        pr_number: &str,
    ) -> Result<(), OrbitError> {
        self.ensure_candidate_identity(status, pr_number)?;
        ensure_matches(
            pr_number,
            "headRefOid",
            status.head_sha.as_deref(),
            self.candidate_sha.as_deref(),
        )
    }

    /// The gate the guarded `review -> done` transition runs behind.
    ///
    /// A merged state is only delivery when the provider also names the merge
    /// commit and the head it merged is the candidate this run authorized.
    pub(in crate::executor::automation::vcs) fn ensure_delivered(
        &self,
        status: &PullRequestStatus,
        pr_number: &str,
    ) -> Result<DeliveryEvidence, OrbitError> {
        self.ensure_pinned_candidate(status, pr_number)?;

        let merge_commit = status.merge_commit.clone().ok_or_else(|| {
            OrbitError::Execution(format!(
                "pr_complete: pull request #{pr_number} reports a merged state without a merge \
                 commit, so this run has no evidence of what landed; the task stays in review"
            ))
        })?;

        Ok(DeliveryEvidence {
            pr_number: pr_number.to_string(),
            merged_at: status.merged_at.clone().unwrap_or_default(),
            head_ref: status.head_branch.clone(),
            base_ref: status.base_branch.clone(),
            head_sha: status.head_sha.clone(),
            merge_commit,
        })
    }

    /// Report measured local ancestry beside a blocked completion.
    ///
    /// Local candidate/base ancestry alone does not establish which merge
    /// commit CI tested: a branch can stay behind while CI tests a merge
    /// with the current base, and checks can be rerun. Without CI run or
    /// tested merge-commit provenance, this note states only the measured
    /// ancestry and that uncertainty. The answer is best effort: it needs a
    /// readable local checkout, and a refusal is never withheld because the
    /// diagnosis was unavailable.
    pub(in crate::executor::automation::vcs) fn base_advance_note(
        &self,
        input: &Value,
        workspace_path: &str,
    ) -> Option<String> {
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
                 ({base_sha}) as measured locally; no CI run or tested merge-commit \
                 provenance was read, so this alone does not establish which merge CI tested."
            )
        } else {
            format!(
                " The published candidate {candidate_sha} is {} commits behind base '{base}' \
                 ({base_sha}) as measured locally; no CI run or tested merge-commit \
                 provenance was read, so this alone does not establish that red required \
                 checks ran against a stale merge ref.",
                freshness.commits_behind
            )
        })
    }
}

impl DeliveryEvidence {
    pub(in crate::executor::automation::vcs) fn as_json(&self) -> Value {
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
    pub(in crate::executor::automation::vcs) fn authorization_fragment(&self) -> String {
        format!(
            "delivered by pull request #{} merged as {}",
            self.pr_number, self.merge_commit
        )
    }
}

/// Compare one reported identity field against its pin.
///
/// An unpinned field is not checked. A pinned field the provider does not
/// report is a refusal rather than a pass, because completion cannot claim
/// an identity it was unable to read.
fn ensure_matches(
    pr_number: &str,
    field: &str,
    reported: Option<&str>,
    pinned: Option<&str>,
) -> Result<(), OrbitError> {
    let Some(pinned) = pinned else {
        return Ok(());
    };
    let Some(reported) = reported else {
        return Err(OrbitError::DeliveryRefused {
            refusal: DeliveryRefusal::EvidenceStale,
            message: format!(
                "delivery_evidence_stale: pull request #{pr_number} did not report {field}, so \
             the authorized candidate '{pinned}' cannot be confirmed; the task stays in review"
            ),
        });
    };
    if reported != pinned {
        let message = format!(
            "delivery_evidence_stale: pull request #{pr_number} reports {field} '{reported}' \
             but this run published '{pinned}'; completion delivers only the candidate it \
             published, so the task stays in review"
        );
        return Err(if field == "headRefOid" {
            OrbitError::DeliveryRefused {
                refusal: DeliveryRefusal::HeadChanged,
                message,
            }
        } else {
            OrbitError::DeliveryRefused {
                refusal: DeliveryRefusal::EvidenceStale,
                message,
            }
        });
    }
    Ok(())
}
