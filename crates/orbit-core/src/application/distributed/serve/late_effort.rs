//! A follower's late report of the reasoning effort a leaf it already
//! settled ran at [ORB-24115], and of its implementing model [ORB-24150].
//!
//! A follower that settled before it reported effort (ORB-23825) or model
//! holds the leaf's implementing invocation and its dispatch identity; this
//! owner holds only the settlement, or nothing at all for a claim that
//! settled before the owner kept settlement records. The follower reads the
//! effort and model from its own records, by the rule it reports at
//! settlement, and sends them here. The owner keeps each beside the claim and
//! its claim inspection fills the settlement's absent effort and model from
//! them, so the scorecards count them like a reported settlement.

use orbit_common::OrbitError;
use orbit_store::contracts::{LateLeafEffortWrite, LateLeafModelWrite};
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
    /// The same for the model, when the report carried one. A follower
    /// reads its absence as an owner that predates the model report.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_outcome: Option<&'static str>,
}

/// The longest model or crew name a late report may carry.
const MODEL_NAME_LIMIT: usize = 200;

impl OrbitRuntime {
    /// Record `effort` (`None` for the provider default) as the effort the
    /// settled claim `claim_id`'s leaf `run_id` ran at and, when given,
    /// `model` as its implementing model. A report without a crew, as a
    /// follower that predates the crew field sends, is recorded as before.
    pub fn serve_claim_effort(
        &self,
        session: &ToolSessionContext,
        claim_id: &str,
        run_id: &str,
        effort: Option<String>,
        model: Option<String>,
    ) -> Result<LateEffortReportResult, OrbitError> {
        self.serve_claim_effort_with_crew(session, claim_id, run_id, effort, None, model)
    }

    /// [`Self::serve_claim_effort`] with the `crew` the leaf run used
    /// [ORB-24206], reported with the same effort and model by the session's
    /// machine. Refused unless that machine ran the claim and bound `run_id` to
    /// it, and refused as `effort_conflict` or `model_conflict` when the claim
    /// already holds a different value, which stands; a refusal records
    /// neither. Attribution only: no claim, task, run or settlement outcome
    /// changes.
    pub fn serve_claim_effort_with_crew(
        &self,
        session: &ToolSessionContext,
        claim_id: &str,
        run_id: &str,
        effort: Option<String>,
        crew: Option<String>,
        model: Option<String>,
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
        // Attribution only: an overlong or empty crew is dropped, never a
        // refusal, so the report stays accepted as it was before the field.
        let crew = crew
            .map(|crew| crew.trim().to_string())
            .filter(|crew| !crew.is_empty() && crew.len() <= MODEL_NAME_LIMIT);
        let model = model
            .map(|model| model.trim().to_string())
            .filter(|model| !model.is_empty());
        if model
            .as_deref()
            .is_some_and(|model| model.len() > MODEL_NAME_LIMIT)
        {
            return Err(OrbitError::InvalidInput(format!(
                "`crew_model` is longer than {MODEL_NAME_LIMIT} bytes"
            )));
        }
        let claim = self.current_claim(claim_id)?;
        let write = self.stores().tasks().record_late_leaf_report(
            &claim.claim_id,
            &machine,
            run_id.trim(),
            effort.clone(),
            crew,
            model.clone(),
        )?;
        let outcome = match write.effort {
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
        if let Some(LateLeafModelWrite::Conflict { held }) = &write.model {
            tracing::warn!(
                claim_id = %claim.claim_id,
                reported = model.as_deref().unwrap_or_default(),
                %held,
                "late model report refused: the claim already holds another model"
            );
            return Err(refusal::refused(format!(
                "model_conflict: the claim already holds model {held}; the report of {} is \
                 refused and the held model stands",
                model.as_deref().unwrap_or_default()
            )));
        }
        let model_outcome = write.model.map(|write| match write {
            LateLeafModelWrite::Recorded => "recorded",
            _ => "replayed",
        });
        Ok(LateEffortReportResult {
            claim_id: claim.claim_id,
            outcome,
            model_outcome,
        })
    }
}
