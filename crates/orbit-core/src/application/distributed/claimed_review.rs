//! Claimed-leaf review ledgers travel with the settlement [ORB-16699].
//!
//! The follower holds the attempt ledger and the owner holds the task.
//! Settlement copies the follower's ledgers onto the owner, which reads them
//! to tell an exhausted claimed review from any other escalation.
//!
//! A claim is one delivery attempt and keys its own review lineage, so an
//! exhausted budget is never reset and resumed: generic resume cannot carry
//! the claim's binding to a new run, and the worker refuses a claimed
//! definition without one. An escalated final recovery for an exhausted
//! lineage instead names the owner's `orbit task requeue`. The task's next
//! claim continues the candidate this claim kept, with a review budget of
//! its own, under a fresh owner admission and worker binding.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_store::contracts::{ClaimEvidence, ClaimedReviewCarry, ExecutionClaim};
use orbit_types::workflow::{FinalRecoveryDecision, ReviewLedger};

use crate::OrbitRuntime;

/// Reason the generated requeue command records. The regression runs that
/// command verbatim, so this text is part of the operator contract.
const CLAIMED_REVIEW_REQUEUE_REASON: &str =
    "requeue after the claimed leaf exhausted its review budget";

impl OrbitRuntime {
    /// Ledgers this follower holds for the claim bound to `run_id`.
    ///
    /// Empty when the run has no local pull, the pull has no claim, or the
    /// review store has no lineage whose key ends with that claim. A store
    /// error is logged and treated as no carry: settlement still proceeds.
    pub(crate) fn claimed_review_carry(&self, run_id: &str) -> Option<ClaimedReviewCarry> {
        let record = match self.stores().jobs().local_pull_for_run(run_id) {
            Ok(record) => record?,
            Err(error) => {
                tracing::warn!(
                    target: "orbit.distributed.claimed_review",
                    run_id,
                    %error,
                    "could not read the local pull while copying its review ledgers",
                );
                return None;
            }
        };
        let claim = record.receipt.as_ref()?.claim.as_ref()?;
        let ledgers = self.owner_review_ledgers_for_claim(&claim.claim_id, &claim.task_id);
        if ledgers.is_empty() {
            return None;
        }
        Some(ClaimedReviewCarry {
            executor_machine_id: record.destination.execution_machine_id.clone(),
            executor_data_root: self.global_root().display().to_string(),
            executor_workspace_id: self.command_workspace_selector().unwrap_or_default(),
            ledgers,
        })
    }

    /// Workspace selector a CLI `--workspace` flag accepts for this runtime.
    ///
    /// The logical catalog id is what the flag resolves. The task-store
    /// partition is the fallback, and `workspace init` writes them as the
    /// same id. Empty when neither can be read.
    fn command_workspace_selector(&self) -> Option<String> {
        self.workspace_runtime_binding()
            .map(|binding| binding.logical_workspace_id.trim().to_string())
            .filter(|id| !id.is_empty())
            .or_else(|| self.workspace_id().ok())
            .filter(|id| !id.is_empty())
    }

    /// Ledgers stored in this workspace for one claim. Errors log and yield
    /// an empty list so a settlement is not failed by the read.
    fn owner_review_ledgers_for_claim(&self, claim_id: &str, task_id: &str) -> Vec<ReviewLedger> {
        let workspace_id = match self.workspace_id() {
            Ok(workspace_id) => workspace_id,
            Err(error) => {
                tracing::warn!(
                    target: "orbit.distributed.claimed_review",
                    %error,
                    "could not resolve the workspace while reading claim review ledgers",
                );
                return Vec::new();
            }
        };
        let store = match self.review_store() {
            Ok(store) => store,
            Err(error) => {
                tracing::warn!(
                    target: "orbit.distributed.claimed_review",
                    %error,
                    "could not open the review store while reading claim review ledgers",
                );
                return Vec::new();
            }
        };
        match store.review_ledgers_for_claim(&workspace_id, claim_id, task_id) {
            Ok(ledgers) => ledgers,
            Err(error) => {
                tracing::warn!(
                    target: "orbit.distributed.claimed_review",
                    claim_id,
                    task_id,
                    %error,
                    "could not read review ledgers for a claim",
                );
                Vec::new()
            }
        }
    }

    /// Validate the whole carry before committing either settlement or ledger
    /// writes. A mixed carry must not partially import its matching ledgers.
    pub(super) fn validate_claimed_review_carry(
        &self,
        claim: &ExecutionClaim,
        carry: Option<&ClaimedReviewCarry>,
    ) -> Result<(), OrbitError> {
        let Some(carry) = carry else {
            return Ok(());
        };
        if carry.executor_machine_id != claim.executed_on.machine_id
            || carry.ledgers.iter().any(|ledger| {
                !ledger_matches(ledger, &claim.task_id, &claim.claim_id)
                    || ledger.task_ids.iter().any(|id| id != &claim.task_id)
            })
        {
            return Err(OrbitError::InvalidInput(
                "claimed review carry differs from the admitted task, claim or executor".into(),
            ));
        }
        Ok(())
    }

    /// Import a validated carry only after the journal accepts the settlement
    /// or its exact replay. A failure is logged; the settlement still stands.
    /// Replaying can finish an interrupted import without undoing owner resets.
    pub(super) fn import_claimed_review_ledgers(&self, carry: Option<&ClaimedReviewCarry>) {
        let Some(carry) = carry else {
            return;
        };
        let Ok(workspace_id) = self.workspace_id() else {
            tracing::warn!(
                target: "orbit.distributed.claimed_review",
                "could not resolve the workspace while importing claimed review ledgers",
            );
            return;
        };
        let Ok(store) = self.review_store() else {
            tracing::warn!(
                target: "orbit.distributed.claimed_review",
                "could not open the review store while importing claimed review ledgers",
            );
            return;
        };
        for ledger in &carry.ledgers {
            if let Err(error) = store.review_import_claimed_ledger(&workspace_id, ledger) {
                tracing::warn!(
                    target: "orbit.distributed.claimed_review",
                    lineage = %ledger.lineage_key,
                    %error,
                    "could not import a claimed review ledger",
                );
            }
        }
    }

    /// For an exhausted claimed-leaf escalation, replace the human action with
    /// the owner's requeue. This preparation has no store writes.
    pub(super) fn prepare_claimed_review_settlement(
        &self,
        claim: &ExecutionClaim,
        mut evidence: ClaimEvidence,
    ) -> ClaimEvidence {
        if let Some(action) = self.claimed_review_requeue_action(claim, &evidence)
            && let Some(recovery) = evidence.final_recovery.as_mut()
            && let FinalRecoveryDecision::Escalate { human_action, .. } = &mut recovery.decision
        {
            *human_action = action;
        }
        evidence
    }

    fn claimed_review_requeue_action(
        &self,
        claim: &ExecutionClaim,
        evidence: &ClaimEvidence,
    ) -> Option<String> {
        let recovery = evidence.final_recovery.as_ref()?;
        let FinalRecoveryDecision::Escalate {
            diagnosis,
            human_action,
        } = &recovery.decision
        else {
            return None;
        };
        let carry = evidence.claimed_review.as_ref()?;
        let matching: Vec<&ReviewLedger> = carry
            .ledgers
            .iter()
            .filter(|ledger| ledger_matches(ledger, &claim.task_id, &claim.claim_id))
            .collect();
        if matching.is_empty() {
            return None;
        }
        let reason_hit = evidence
            .failure
            .as_ref()
            .is_some_and(|failure| failure.reason.contains("review_budget_exhausted"));
        let text_hit = mentions_review_reset(diagnosis) || mentions_review_reset(human_action);
        let exhausted = matching.iter().any(|ledger| ledger_exhausted(ledger));
        if !reason_hit && !text_hit && !exhausted {
            return None;
        }
        let kept_candidate = evidence
            .failure
            .as_ref()
            .is_some_and(|failure| failure.candidate.is_some());
        let owner_machine = self.distributed_owner_machine_id()?;
        let owner_root = self.global_root();
        let owner_workspace = self.command_workspace_selector()?;
        Some(executable_requeue_action(
            &owner_machine,
            &owner_root,
            &owner_workspace,
            &claim.task_id,
            kept_candidate,
        ))
    }
}

fn ledger_matches(ledger: &ReviewLedger, task_id: &str, claim_id: &str) -> bool {
    ledger.budget.minutes > 0
        && ledger.task_ids.iter().any(|id| id == task_id)
        && ledger.lineage_key.ends_with(&format!("/{claim_id}"))
}

fn ledger_exhausted(ledger: &ReviewLedger) -> bool {
    ledger.decisions.is_empty()
        && ledger.consumed_seconds >= u64::from(ledger.budget.minutes).saturating_mul(60)
}

fn mentions_review_reset(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("review reset")
        || lower.contains("review-reset")
        || lower.contains("review_budget")
}

fn executable_requeue_action(
    owner_machine_id: &str,
    owner_global_root: &Path,
    owner_workspace: &str,
    task_id: &str,
    kept_candidate: bool,
) -> String {
    let owner_root = shell_single(&owner_global_root.display().to_string());
    let owner_workspace = shell_single(owner_workspace);
    let reason = shell_single(CLAIMED_REVIEW_REQUEUE_REASON);
    let next = if kept_candidate {
        "continues the candidate this claim kept"
    } else {
        "implements it afresh, since this claim kept no candidate"
    };
    // `--root` alone pins the data directory and selects no checkout.
    // `--workspace` names the registered workspace, so the command works
    // from any working directory on that host.
    format!(
        "On host `{owner_machine_id}`, run `ORBIT_OPERATOR=1 orbit --root {owner_root} --workspace {owner_workspace} task requeue {task_id} --reason {reason}`. The task's next claim {next} with a review budget of its own. Do not resume the failed leaf: a claim is one attempt, and job resume refuses a claimed run."
    )
}

/// Wrap `value` in single quotes, escaping embedded quotes for `sh`.
fn shell_single(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
