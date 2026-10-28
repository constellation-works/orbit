//! Independent head verification before settling a stopped authorized handoff.

use orbit_common::OrbitError;
use orbit_engine::PullRequestStatus;
use orbit_store::contracts::{ClaimInvocation, HandoffObservation};
use serde_json::json;

use super::{MergedPullRequest, RevokedHandoff};
use crate::OrbitRuntime;

impl OrbitRuntime {
    pub(super) fn external_handoff_settlement_context(
        &self,
        handoff: &RevokedHandoff,
        landing: &MergedPullRequest,
        context: ClaimInvocation,
    ) -> Result<ClaimInvocation, OrbitError> {
        let Some(candidate) = &handoff.candidate else {
            return Ok(context);
        };
        let status = PullRequestStatus::from_status_response(&json!({"pull_request":{
            "number":landing.pull.number,
            "url":format!("https://github.com/{}/pull/{}", candidate.repository, landing.pull.number),
            "state":"MERGED", "headRefName":landing.pull.head_branch,
            "baseRefName":landing.base_branch,"headRefOid":landing.head_sha,
            "mergeCommit":{"oid":landing.merge_commit},
        }}));
        orbit_engine::verify_handoff_pull_request_head(&self.paths().repo_root, candidate, &status)
            .map_err(|error| OrbitError::Execution(format!(
                "external merge does not verify the accepted handoff: {error}; reconcile_review is required for a merged head with other changes"
            )))?;
        let (_, accepted) = self.landing_claim(&handoff.handoff_id)?;
        if accepted.handoff.candidate != *candidate {
            return Err(OrbitError::Execution(
                "accepted handoff changed during external merge settlement".into(),
            ));
        }
        Ok(context.with_handoff_observation(HandoffObservation {
            candidate: candidate.clone(),
            footprint_widening: accepted.handoff.footprint_widening,
            required_commands: accepted.required_commands,
            owner_completion_authority: self.owner_completion_authority(),
            review: None,
            known_red: Vec::new(),
            light_delivery: self.owner_light_delivery(&accepted.handoff.task_id, candidate)?,
        }))
    }
}
