//! Claimed-leaf review ledgers travel with the settlement [ORB-16699].
//!
//! The follower holds the attempt ledger. The owner holds the task, so
//! `review-reset` on the owner used to report that the lineage had no
//! ledger, and the follower has no task record to reset. Settlement copies
//! the follower's ledgers onto the owner. An escalated final recovery then
//! names the owner command that resets that ledger and the follower command
//! that resumes the run. Resume adopts the owner's reset back onto the
//! follower before it will accept a claimed run.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_store::contracts::{
    ClaimEvidence, ClaimedReviewCarry, ExecutionClaim, LocalPullAdmission,
};
use orbit_types::workflow::{FinalRecoveryDecision, ReviewLedger};

use crate::OrbitRuntime;

/// Reason the generated reset command records. The regression runs that
/// command verbatim, so this text is part of the operator contract.
pub(crate) const CLAIMED_REVIEW_RESET_REASON: &str =
    "operator reset of exhausted claimed-leaf review budget";

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
    /// an empty list so a lookup or a settlement is not failed by the read.
    pub(super) fn owner_review_ledgers_for_claim(
        &self,
        claim_id: &str,
        task_id: &str,
    ) -> Vec<ReviewLedger> {
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
    /// the owner reset and follower resume. This preparation has no store writes.
    pub(super) fn prepare_claimed_review_settlement(
        &self,
        claim: &ExecutionClaim,
        mut evidence: ClaimEvidence,
    ) -> ClaimEvidence {
        if let Some(action) = self.claimed_review_reset_action(claim, &evidence)
            && let Some(recovery) = evidence.final_recovery.as_mut()
            && let FinalRecoveryDecision::Escalate { human_action, .. } = &mut recovery.decision
        {
            *human_action = action;
        }
        evidence
    }

    /// Copy an owner reset back onto this follower. True when a matching
    /// ledger here, or one the owner lookup returned, already has an
    /// operator decision. Transport and store failures log and return false,
    /// which keeps the generic claimed-resume refusal.
    pub(crate) fn adopt_claimed_review_reset(&self, pull: &LocalPullAdmission) -> bool {
        let Some(claim) = pull
            .receipt
            .as_ref()
            .and_then(|receipt| receipt.claim.as_ref())
        else {
            return false;
        };
        let local = self.owner_review_ledgers_for_claim(&claim.claim_id, &claim.task_id);
        if local.iter().any(|ledger| !ledger.decisions.is_empty()) {
            return true;
        }
        let Some(transport) = self.drain_owner_transport() else {
            tracing::warn!(
                target: "orbit.distributed.claimed_review",
                claim_id = %claim.claim_id,
                "no owner transport; a claimed review reset cannot be adopted",
            );
            return false;
        };
        let request_id = pull.request.request_id.as_str();
        let value = match transport.call(
            &pull.destination.selector,
            "orbit.drain.receipt.lookup",
            serde_json::json!({ "request_id": request_id }),
        ) {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!(
                    target: "orbit.distributed.claimed_review",
                    request_id,
                    %error,
                    "owner receipt lookup failed while adopting a claimed review reset",
                );
                return false;
            }
        };
        let ledgers = match value.get("review_ledgers").cloned() {
            Some(raw) => match serde_json::from_value::<Vec<ReviewLedger>>(raw) {
                Ok(ledgers) => ledgers,
                Err(error) => {
                    tracing::warn!(
                        target: "orbit.distributed.claimed_review",
                        request_id,
                        %error,
                        "owner receipt lookup returned review ledgers that could not be read",
                    );
                    return false;
                }
            },
            None => Vec::new(),
        };
        let Ok(workspace_id) = self.workspace_id() else {
            return false;
        };
        let Ok(store) = self.review_store() else {
            return false;
        };
        let mut adopted = false;
        for ledger in ledgers {
            if !ledger_matches(&ledger, &claim.task_id, &claim.claim_id) {
                continue;
            }
            match store.review_import_claimed_ledger(&workspace_id, &ledger) {
                Ok(imported) if !imported.decisions.is_empty() => adopted = true,
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(
                        target: "orbit.distributed.claimed_review",
                        lineage = %ledger.lineage_key,
                        %error,
                        "could not adopt a claimed review ledger from the owner",
                    );
                }
            }
        }
        adopted
    }

    fn claimed_review_reset_action(
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
        if carry.executor_machine_id.is_empty() || carry.executor_data_root.is_empty() {
            return None;
        }
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
        let chosen = matching
            .iter()
            .copied()
            .filter(|ledger| ledger_exhausted(ledger))
            .max_by_key(|ledger| ledger.consumed_seconds)
            .or_else(|| matching.first().copied())?;
        let owner_machine = self.distributed_owner_machine_id()?;
        let owner_root = self.global_root();
        let owner_workspace = self.command_workspace_selector()?;
        let follower_workspace = follower_workspace_selector(carry, &chosen.lineage_key)?;
        Some(executable_reset_action(
            &owner_machine,
            &owner_root,
            &owner_workspace,
            &carry.executor_machine_id,
            &carry.executor_data_root,
            &follower_workspace,
            &claim.task_id,
            &chosen.lineage_key,
            &recovery.run_id,
        ))
    }
}

/// Workspace the follower's resume command must select.
///
/// The carry names the logical id. A carry from before that field uses the
/// lineage key's first segment, which is the follower partition id and, for
/// a workspace bound by `workspace init`, the same selector.
fn follower_workspace_selector(carry: &ClaimedReviewCarry, lineage_key: &str) -> Option<String> {
    let explicit = carry.executor_workspace_id.trim();
    if !explicit.is_empty() {
        return Some(explicit.to_string());
    }
    let (prefix, _) = lineage_key.split_once('/')?;
    let prefix = prefix.trim();
    (!prefix.is_empty()).then(|| prefix.to_string())
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

#[allow(clippy::too_many_arguments)] // one slot per token of the two operator commands
fn executable_reset_action(
    owner_machine_id: &str,
    owner_global_root: &Path,
    owner_workspace: &str,
    follower_machine_id: &str,
    follower_global_root: &str,
    follower_workspace: &str,
    task_id: &str,
    lineage_key: &str,
    run_id: &str,
) -> String {
    let owner_root = shell_single(&owner_global_root.display().to_string());
    let owner_workspace = shell_single(owner_workspace);
    let lineage = shell_single(lineage_key);
    let reason = shell_single(CLAIMED_REVIEW_RESET_REASON);
    let follower_root = shell_single(follower_global_root);
    let follower_workspace = shell_single(follower_workspace);
    // `--root` alone pins the data directory and selects no checkout.
    // `--workspace` names the registered workspace, so the command works
    // from any working directory on that host.
    format!(
        "On host `{owner_machine_id}`, run `ORBIT_OPERATOR=1 orbit --root {owner_root} --workspace {owner_workspace} task review-reset {task_id} --lineage {lineage} --reason {reason}`. On host `{follower_machine_id}`, run `orbit --root {follower_root} --workspace {follower_workspace} job resume {run_id}`."
    )
}

/// Wrap `value` in single quotes, escaping embedded quotes for `sh`.
fn shell_single(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
