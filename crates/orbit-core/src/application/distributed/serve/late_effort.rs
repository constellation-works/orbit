//! A follower's late report of the reasoning effort a leaf it already
//! settled ran at [ORB-24115].
//!
//! A follower that settled before it reported effort (ORB-23825) holds the
//! leaf's implementing invocation and its dispatch identity; this owner holds
//! only the settlement. The follower reads the effort from its own records,
//! by the rule it reports at settlement, and sends it here. The owner keeps
//! it beside the claim and its claim inspection fills the settlement's absent
//! effort from it, so the scorecards count it like a reported settlement.

use orbit_common::OrbitError;
use orbit_store::contracts::LateLeafEffortWrite;
use orbit_types::tool::ToolSessionContext;
use serde::Serialize;

use crate::OrbitRuntime;
use crate::application::distributed::ensure_distributed_mutation_available;

use super::refusal;

/// The owner's answer to an accepted late effort report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LateEffortReportResult {
    pub claim_id: String,
    /// `recorded`, or `replayed` when the claim already held this effort.
    pub outcome: &'static str,
}

impl OrbitRuntime {
    /// Record `effort` (`None` for the provider default) as the effort the
    /// settled claim `claim_id`'s leaf `run_id` ran at, reported by the
    /// session's machine. Refused unless that machine ran the claim and bound
    /// `run_id` to it, and refused as `effort_conflict` when the claim already
    /// holds a different effort, which stands. Attribution only: no claim,
    /// task, run or settlement outcome changes.
    pub fn serve_claim_effort(
        &self,
        session: &ToolSessionContext,
        claim_id: &str,
        run_id: &str,
        effort: Option<String>,
    ) -> Result<LateEffortReportResult, OrbitError> {
        ensure_distributed_mutation_available("orbit.drain.claim.effort")?;
        self.ensure_distributed_owner_workspace()?;
        let machine = self.session_caller_machine(session)?;
        if let Some(name) = effort.as_deref()
            && name
                .parse::<orbit_types::identity::ReasoningEffort>()
                .is_err()
        {
            return Err(OrbitError::InvalidInput(format!(
                "`reasoning_effort` '{name}' is not a reasoning effort; send low, medium, high, \
                 xhigh, max or provider_default"
            )));
        }
        let claim = self.current_claim(claim_id)?;
        let write = self.stores().tasks().record_late_leaf_effort(
            &claim.claim_id,
            &machine,
            run_id.trim(),
            effort.clone(),
        )?;
        let outcome = match write {
            LateLeafEffortWrite::Recorded => "recorded",
            LateLeafEffortWrite::Replayed => "replayed",
            LateLeafEffortWrite::Conflict { held } => {
                let label = |effort: Option<&str>| effort.unwrap_or("provider_default").to_string();
                tracing::warn!(
                    claim_id = %claim.claim_id,
                    reported = %label(effort.as_deref()),
                    held = %label(held.as_deref()),
                    "late effort report refused: the claim already holds another effort"
                );
                return Err(refusal::refused(format!(
                    "effort_conflict: the claim already holds effort {}; the report of {} is \
                     refused and the held effort stands",
                    label(held.as_deref()),
                    label(effort.as_deref())
                )));
            }
        };
        Ok(LateEffortReportResult {
            claim_id: claim.claim_id,
            outcome,
        })
    }
}
