//! Restore a missing reference without acquiring new lifecycle authority.

use std::collections::BTreeSet;

use orbit_common::OrbitError;
use orbit_types::task::{ExternalRef, GITHUB_PR_EXTERNAL_REF_SYSTEM, TaskStatus};
use orbit_types::workflow::handoff::HandoffDelivery;

use super::super::lifecycle::invalid;
use super::TaskCommitBoundary;
use crate::contracts::{
    ClaimCommitEffects, ClaimWorkerUpdate, TaskCoordinationCommitOutcome,
    TaskCoordinationCommitParams,
};

impl TaskCommitBoundary {
    /// Repair only a review task's missing PR reference, using the owner's
    /// accepted handoffs as evidence. Ambiguous deliveries are refused. This
    /// narrow journal write preserves all claim, reservation and landing state.
    pub fn repair_missing_handoff_pr_ref(&self, task_id: &str) -> Result<bool, OrbitError> {
        self.with_admission(|| {
            self.flush_deferred_apply(task_id)?;
            let bundle = self.bundle_store.read_bundle_lightweight(task_id)?;
            if bundle.envelope.status != TaskStatus::Review
                || bundle.envelope.external_refs.iter().any(|reference| {
                    reference.system == GITHUB_PR_EXTERNAL_REF_SYSTEM
                })
            {
                return Ok(false);
            }
            let deliveries: BTreeSet<_> = self.accepted_handoffs()?.into_iter()
                .filter(|accepted| accepted.handoff.task_id == task_id)
                .filter_map(|accepted| match accepted.handoff.candidate.delivery {
                    HandoffDelivery::PullRequest { number } => Some((number, accepted.handoff.candidate.repository)),
                    _ => None,
                })
                .collect();
            if deliveries.len() > 1 {
                return Err(invalid(&format!(
                    "multiple accepted pull requests for task {task_id}; inspect its handoffs before repairing the reference"
                )));
            }
            let Some((number, repository)) = deliveries.first() else {
                return Ok(false);
            };
            let params = TaskCoordinationCommitParams {
                task_id: task_id.into(),
                actor: "system".into(),
                expected_status: vec![TaskStatus::Review],
                status_event: Some("pull_request_ref_repaired".into()),
                status_note: Some(format!("restored pull request #{number} from accepted follower handoff")),
                ..Default::default()
            };
            let effects = ClaimCommitEffects {
                worker_update: Some(ClaimWorkerUpdate {
                    external_refs: vec![ExternalRef::github_pr_with_url(number.to_string(), Some(&format!("https://github.com/{repository}/pull/{number}")))?],
                    ..Default::default()
                }),
                ..Default::default()
            };
            match self.commit_locked_effects(
                &params, &mut |_| Ok(Vec::new()), &effects, &Default::default(), None,
            )? {
                TaskCoordinationCommitOutcome::Committed(_) => Ok(true),
                _ => Err(invalid("task changed during pull request reference repair")),
            }
        })
    }
}
