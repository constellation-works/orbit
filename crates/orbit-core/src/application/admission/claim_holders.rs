//! The live execution claims behind a lock wait [ORB-15514].
//!
//! A claimed task is `in-progress` on the owner, so the drain and readiness
//! already see its footprint as an ordinary held lock. What they could not
//! say is that a follower holds it: the remedy is that claim settling, on
//! another machine, not a local run finishing. This names the claim wherever
//! a wait names its blocking task.

use std::collections::BTreeMap;

use orbit_store::contracts::ExecutionClaim;
use serde_json::{Value, json};

use crate::OrbitRuntime;

/// Each task a live execution claim holds, with that claim.
#[derive(Debug, Default)]
pub(crate) struct ClaimHolders {
    by_task: BTreeMap<String, ExecutionClaim>,
}

impl ClaimHolders {
    /// The claims that protect a footprint now. Diagnostic only: a journal
    /// that cannot be read leaves the waits unannotated rather than failing
    /// the drain pass or the readiness read that would report them.
    pub(crate) fn read(runtime: &OrbitRuntime) -> Self {
        match runtime.inspect_execution_claims() {
            Ok(claims) => Self {
                by_task: claims
                    .into_iter()
                    .map(|inspection| inspection.claim)
                    .filter(|claim| claim.phase.protects_footprint())
                    .map(|claim| (claim.task_id.clone(), claim))
                    .collect(),
            },
            Err(error) => {
                tracing::warn!("lock waits do not name execution claims: {error}");
                Self::default()
            }
        }
    }

    /// The claims holding `task_ids`, as readiness reports them.
    pub(crate) fn json_for<'a>(&self, task_ids: impl IntoIterator<Item = &'a str>) -> Vec<Value> {
        self.held(task_ids).map(claim_json).collect()
    }

    /// One line naming the claims holding `task_ids`, or `None` when no live
    /// claim holds any of them.
    pub(crate) fn detail_for<'a>(
        &self,
        task_ids: impl IntoIterator<Item = &'a str>,
    ) -> Option<String> {
        let details = self.held(task_ids).map(claim_detail).collect::<Vec<_>>();
        (!details.is_empty()).then(|| details.join("; "))
    }

    fn held<'a, 'b>(
        &'a self,
        task_ids: impl IntoIterator<Item = &'b str>,
    ) -> impl Iterator<Item = &'a ExecutionClaim> {
        let mut task_ids = task_ids.into_iter().collect::<Vec<_>>();
        task_ids.sort_unstable();
        task_ids.dedup();
        task_ids
            .into_iter()
            .filter_map(|task_id| self.by_task.get(task_id))
    }
}

/// What a wait says about the claim that holds its blocker.
pub(crate) fn claim_detail(claim: &ExecutionClaim) -> String {
    let location = &claim.executed_on;
    format!(
        "{} is held by a live execution claim on {} (claim {}, run {})",
        claim.task_id,
        location
            .machine_name
            .as_deref()
            .unwrap_or(location.machine_id.as_str()),
        claim.claim_id,
        claim.run_context.run_id,
    )
}

pub(crate) fn claim_json(claim: &ExecutionClaim) -> Value {
    json!({
        "task_id": claim.task_id,
        "claim_id": claim.claim_id,
        "machine_id": claim.executed_on.machine_id,
        "machine_name": claim.executed_on.machine_name,
        "run_id": claim.run_context.run_id,
        "phase": claim.phase,
    })
}
