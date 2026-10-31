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
use orbit_types::workflow::DeliveryRefusal;
use serde_json::{Value, json};

use super::super::super::input::input_string_field;
use super::super::freshness::{branch_freshness_against_ref, commit_sha};
use super::super::git::{base_sync_mode_from_input, resolve_worktree_start_point};
use super::super::operations;
use super::answers::{CheckState, PullRequestStatus, RequiredChecks};
use super::auto_fix::UNSETTLED_BLOCKED_POLLS;
use super::merge_state::{BlockedCheckRefusal, blocked_checks, has_failed_pr_check};
pub(in crate::executor::automation::vcs) use super::merge_state::{
    PrMergeState, classify_pr_state,
};
use crate::context::RuntimeHost;

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
    pub(super) fn previous_candidate_sha(&self) -> Option<&str> {
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

    /// Explain whether red required checks describe the current candidate/base.
    ///
    /// GitHub runs required checks on a merge of the head into the base as it
    /// stood when the check started. Once the base advances past that point the
    /// red result describes a merge ref nobody is proposing any more — the
    /// exact confusion F2026-09-102 recorded, where rebasing onto the current
    /// base turned the same suite green. The answer is best effort: it needs a
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

/// Which consumer may refine a failed rollup into a bounded required-check wait.
#[derive(Clone, Copy)]
pub(in crate::executor::automation::vcs) enum BlockedCheckWait<'a> {
    /// Owner landing retains its failed-rollup refusal and captured owed checks.
    Owner(&'a [String]),
    /// Auto-fix classifies failures against policy before deciding to recover.
    AutoFix,
}

/// [ORB-16339] Why the last observation of a BLOCKED head waited although the
/// head reported nothing pending and nothing failed.
enum UnreportedWait {
    None,
    /// Current branch policy names these checks, and the head has not
    /// reported them yet.
    Missing(Vec<String>),
    /// Branch policy could not be read, so a bounded grace stands in for it.
    Unsettled(String),
}

/// Required checks a freshly published head has not reported yet, tracked
/// across one completion or landing wait. A head can read BLOCKED for a few
/// seconds before the provider queues its first check run.
pub(in crate::executor::automation::vcs) struct UnreportedChecks {
    last: UnreportedWait,
    /// Latest required-check observation; diagnostic only, never authority.
    pub(in crate::executor::automation::vcs) required_observation: Value,
    policy_observation: Value,
    /// Observations waited on while branch policy was unreadable.
    unsettled_polls: u32,
}

impl UnreportedChecks {
    pub(in crate::executor::automation::vcs) fn new() -> Self {
        Self {
            last: UnreportedWait::None,
            required_observation: json!({"state":"not_collected"}),
            policy_observation: json!({"state":"not_collected"}),
            unsettled_polls: 0,
        }
    }

    /// Whether the last observation waits for checks readable policy names.
    /// Absent-context recovery may still diagnose such a head.
    pub(in crate::executor::automation::vcs) fn awaiting_policy_checks(&self) -> bool {
        matches!(self.last, UnreportedWait::Missing(_))
    }

    /// The existing branch-policy refinement, if it ran. This retains only
    /// what this wait already parsed; it is not a policy payload or emission proof.
    pub(in crate::executor::automation::vcs) fn diagnostic(&self) -> Value {
        json!({"awaiting_policy_checks":self.awaiting_policy_checks(),
            "policy_read":self.policy_observation})
    }

    /// What a wait that ends here was still waiting for, for its stop reason.
    pub(in crate::executor::automation::vcs) fn timeout_note(&self) -> String {
        match &self.last {
            UnreportedWait::None => String::new(),
            UnreportedWait::Missing(names) => format!(
                " (required checks the head has not reported: {})",
                names.join(", ")
            ),
            UnreportedWait::Unsettled(why) => format!(
                " (the head reported no pending required check and branch policy is \
                 unreadable: {why})"
            ),
        }
    }
}

/// Why a BLOCKED head's check wait could not be refined. Displays as its
/// reason; `cause` is the provider read that failed, when one did, so a stop
/// built from the reason keeps that failure's class.
pub(in crate::executor::automation::vcs) struct CheckWaitRefusal {
    reason: String,
    cause: Option<Box<OrbitError>>,
    refusal: Option<DeliveryRefusal>,
}

impl CheckWaitRefusal {
    /// The error `wrap` renders from the reason, carrying the class of the
    /// failed provider read behind it.
    pub(in crate::executor::automation::vcs) fn into_error(
        self,
        wrap: impl FnOnce(&str) -> OrbitError,
    ) -> OrbitError {
        let wrapper = wrap(&self.reason);
        let refusal = self.refusal.or(match self.cause.as_deref() {
            Some(OrbitError::DeliveryRefused { refusal, .. }) => Some(*refusal),
            _ => None,
        });
        if let Some(refusal) = refusal {
            return match wrapper {
                OrbitError::Execution(message) => OrbitError::DeliveryRefused { refusal, message },
                other => other,
            };
        }
        match &self.cause {
            Some(cause) => operations::carry_class(cause, wrapper),
            None => wrapper,
        }
    }
}

impl From<String> for CheckWaitRefusal {
    fn from(reason: String) -> Self {
        Self {
            reason,
            cause: None,
            refusal: None,
        }
    }
}

impl From<BlockedCheckRefusal> for CheckWaitRefusal {
    fn from(reason: BlockedCheckRefusal) -> Self {
        Self {
            reason: reason.to_string(),
            cause: None,
            refusal: reason.delivery_refusal(),
        }
    }
}

impl From<&str> for CheckWaitRefusal {
    fn from(reason: &str) -> Self {
        reason.to_string().into()
    }
}

impl std::fmt::Display for CheckWaitRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.reason)
    }
}

/// Require green required checks even on a provider-reported mergeable head.
/// Refine BLOCKED only from authenticated current policy on the observed head,
/// or an actually running check captured as owner-owed. A completed
/// neutral/skipped result is never pending. An absent check is pending only
/// while current branch policy names it as required [ORB-16339]; when that
/// policy is unreadable, owner consumers get the bounded unsettled-poll grace
/// auto-fix already applies. This grants only another bounded observation
/// within the caller's deadline; only the provider's mergeable state can merge.
/// `None` performs the bounded immediate read used by zero-wait completion;
/// a watch always passes its original absolute deadline.
pub(in crate::executor::automation::vcs) fn checked_pr_state<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace: &Path,
    pr: &str,
    status: &PullRequestStatus,
    wait: BlockedCheckWait<'_>,
    unreported: &mut UnreportedChecks,
    deadline_ms: Option<u64>,
) -> Result<PrMergeState, CheckWaitRefusal> {
    unreported.last = UnreportedWait::None;
    unreported.required_observation = json!({"state":"not_collected"});
    unreported.policy_observation = json!({"state":"not_collected"});
    let state = classify_pr_state(status);
    // A draft is already refused. Refining BLOCKED into a check wait would
    // hand it to auto-merge or repair [ORB-16342].
    if status.is_draft
        || !(matches!(state, PrMergeState::Mergeable)
            || (status.merge_state_status.as_deref() == Some("BLOCKED")
                && matches!(state, PrMergeState::Blocked(_))))
    {
        return Ok(state);
    }
    if status.state.as_deref() != Some("OPEN") {
        return Err("pull request state is unavailable for a blocked check wait".into());
    }
    let checks = blocked_checks(status).map_err(|reason| {
        let mut refusal = CheckWaitRefusal::from(reason);
        if has_failed_pr_check(status) {
            refusal.refusal = None;
        }
        refusal
    })?;
    let owed = match wait {
        BlockedCheckWait::Owner(owed) => {
            if !matches!(state, PrMergeState::Mergeable)
                && checks
                    .iter()
                    .any(|check| check.state == Some(CheckState::Failed))
            {
                return Ok(state);
            }
            owed
        }
        BlockedCheckWait::AutoFix => &[],
    };
    let mut required_input = json!({"pr":pr,"workspace_path":workspace.to_string_lossy()});
    if let Some(deadline_ms) = deadline_ms {
        required_input["deadline_ms"] = json!(deadline_ms);
    }
    let response = host.run_private_vcs_operation(operations::PR_REQUIRED_CHECKS, required_input);
    let required = RequiredChecks::observe_response(response, &mut unreported.required_observation)
        .map_err(|error| CheckWaitRefusal {
            reason: format!("required-check evidence is unavailable: {error}"),
            cause: Some(Box::new(error)),
            refusal: None,
        })?;
    if status.head_sha.as_deref() != Some(required.head_sha.as_str())
        || required.reported_head_sha.as_deref() != Some(required.head_sha.as_str())
    {
        unreported.required_observation["outcome"] = json!("head_mismatch");
        return Err("required-check evidence does not identify the current head".into());
    }
    unreported.required_observation["head_matches"] = json!(true);
    let mut pending = false;
    let mut failed = None;
    for check in &required.checks {
        match check.state {
            Some(CheckState::Pending) => pending = true,
            Some(CheckState::Passed)
                if matches!(state, PrMergeState::Mergeable)
                    && check.owed_state != Some(CheckState::Passed)
                    && !required.never_failed(check) =>
            {
                // Branch protection accepts skipped/neutral conclusions, and
                // so does this gate for a check that never failed on this
                // head (a docs-only skip). Neither is a passing rerun of a
                // failed required check, and unreadable history cannot show
                // the check never failed. A refusal also prevents delegating
                // this head to auto-merge.
                failed = Some(format!(
                    "required check '{}' has no successful run on the current head",
                    check.name.as_deref().unwrap_or_default()
                ));
            }
            Some(CheckState::Passed) => {}
            Some(CheckState::Failed) => {
                failed = Some(format!(
                    "required check '{}' failed",
                    check.name.as_deref().unwrap_or_default()
                ));
            }
            None => {
                unreported.required_observation["outcome"] = json!("unreadable_state");
                return Err("required-check evidence is unreadable".into());
            }
        }
    }
    pending |= checks.iter().any(|check| {
        check.state == Some(CheckState::Pending)
            && owed.iter().any(|name| {
                check.name.as_deref() == Some(name.as_str())
                    || check
                        .workflow_name
                        .as_ref()
                        .zip(check.name.as_ref())
                        .is_some_and(|(workflow, own)| name == &format!("{workflow} / {own}"))
            })
    });
    if pending {
        return Ok(PrMergeState::Pending);
    }
    if let Some(reason) = failed {
        return Ok(PrMergeState::Blocked(reason.into()));
    }
    if matches!(state, PrMergeState::Mergeable) {
        return Ok(state);
    }
    // A failed check keeps its existing recovery or stop.
    if checks
        .iter()
        .any(|check| check.state == Some(CheckState::Failed))
    {
        return Ok(state);
    }
    let missing = unreported_required_checks(host, workspace, pr, status, deadline_ms);
    unreported.policy_observation = match &missing {
        Ok(names) => json!({"state":"collected", "missing_checks":
            super::diagnostics::collection(names, |name| super::diagnostics::text(name))}),
        Err(why) => json!({"state":"unreadable", "reason":super::diagnostics::text(why)}),
    };
    match missing {
        Ok(missing) if missing.is_empty() => Ok(state),
        Ok(missing) => {
            unreported.last = UnreportedWait::Missing(missing);
            Ok(PrMergeState::Pending)
        }
        // Auto-fix applies the same grace in its own BLOCKED verdict.
        Err(why) => match wait {
            BlockedCheckWait::Owner(_) if unreported.unsettled_polls < UNSETTLED_BLOCKED_POLLS => {
                unreported.unsettled_polls += 1;
                unreported.last = UnreportedWait::Unsettled(why);
                Ok(PrMergeState::Pending)
            }
            _ => Ok(state),
        },
    }
}

/// The checks current branch policy requires that the observed head has not
/// reported at all, from the same authenticated current-gates read a base
/// update is held to. A check the head reported, in any state, is not here.
fn unreported_required_checks<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace: &Path,
    pr: &str,
    status: &PullRequestStatus,
    deadline_ms: Option<u64>,
) -> Result<Vec<String>, String> {
    let deadline_ms =
        deadline_ms.ok_or("no completion wait budget remains for required-check policy")?;
    let (Some(head), Some(branch), Some(base)) = (
        status.head_sha.as_deref(),
        status.head_branch.as_deref(),
        status.base_branch.as_deref(),
    ) else {
        return Err("the pull request did not report its head and base".into());
    };
    let gates = host
        .run_private_vcs_operation(
            operations::CI_CURRENT_GATES,
            json!({
                "workspace_path":workspace.to_string_lossy(),"pr":pr,"head_sha":head,
                "head_branch":branch,"base_branch":base,"deadline_ms":deadline_ms,
            }),
        )
        .map_err(|error| error.to_string())?;
    gates["missing"]
        .as_array()
        .ok_or_else(|| "the current-gates answer named no missing checks".to_string())?
        .iter()
        .map(|gate| {
            gate["context"]
                .as_str()
                .filter(|context| !context.is_empty())
                .map(ToOwned::to_owned)
                .ok_or_else(|| "the current-gates answer named an unreadable check".to_string())
        })
        .collect()
}

/// [ORB-15677] Where the owner-owed hosted checks stand on a pull request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::executor::automation::vcs) enum OwedHostedChecks {
    /// Every named check reported an explicit success, and none is still
    /// running or finished without one.
    Passed,
    /// A named check has not reported yet, is still running, or completed
    /// without an explicit success (skipped or neutral).
    Waiting(Vec<String>),
    /// A named check failed, or its state is unreadable.
    Failed(String),
}

/// Judge the hosted checks `names` against the pull request's status rollup.
/// A rollup entry answers a name by its own name or as `<workflow> / <job>`,
/// as the forge displays it. A name no entry answers has not reported yet.
///
/// Only an explicit success satisfies a name: a CheckRun conclusion of
/// `SUCCESS`, or a StatusContext state of `SUCCESS`. A completed `SKIPPED`
/// or `NEUTRAL` conclusion is not that analysis, so the owner keeps waiting
/// [ORB-15812]. Branch protection still classifies those conclusions as
/// satisfied.
pub(in crate::executor::automation::vcs) fn owed_hosted_checks(
    pull_request: &PullRequestStatus,
    names: &[String],
) -> OwedHostedChecks {
    if names.is_empty() {
        return OwedHostedChecks::Passed;
    }
    let checks = pull_request.status_checks.as_deref().unwrap_or_default();
    let mut waiting = Vec::new();
    for name in names {
        let mut answered = false;
        let mut pending = false;
        for check in checks {
            let own = check.name.as_deref().unwrap_or_default();
            let qualified = check
                .workflow_name
                .as_deref()
                .map(|workflow| format!("{workflow} / {own}"));
            if own != name && qualified.as_deref() != Some(name.as_str()) {
                continue;
            }
            answered = true;
            match check.owed_state {
                Some(CheckState::Passed) => {}
                Some(CheckState::Pending) => pending = true,
                Some(CheckState::Failed) => {
                    return OwedHostedChecks::Failed(format!("hosted check '{name}' failed"));
                }
                None => {
                    return OwedHostedChecks::Failed(format!(
                        "hosted check '{name}' reports an unreadable state"
                    ));
                }
            }
        }
        if !answered || pending {
            waiting.push(name.clone());
        }
    }
    if waiting.is_empty() {
        OwedHostedChecks::Passed
    } else {
        OwedHostedChecks::Waiting(waiting)
    }
}
