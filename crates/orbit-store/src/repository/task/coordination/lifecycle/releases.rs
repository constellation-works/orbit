use std::collections::BTreeMap;

use chrono::Utc;
use orbit_common::OrbitError;

use super::super::TaskCommitBoundary;
use super::codec::{RELEASE_BUDGET_WINDOW_HOURS, STATE};
use super::decode;
use crate::contracts::*;

impl TaskCommitBoundary {
    /// What a drain run on `machine_id` already gave back for failures that
    /// blame its host [ORB-14257]. Admission keeps each such task from that
    /// drain for the rest of its window — the follower excludes the leaf's
    /// crew too, but a release its worker delivered after the follower built
    /// its next request must not be pulled straight back — and keeps every
    /// task from it once one release blames the host whatever crew runs
    /// there.
    pub(in super::super) fn drain_releases(
        &self,
        machine_id: &str,
        run_id: &str,
    ) -> Result<DrainReleases, OrbitError> {
        let mut released = DrainReleases::default();
        for row in self.coordination_rows(STATE)? {
            let state: ClaimInspection = decode(&row.payload_json)?;
            if state.claim.executed_on.machine_id != machine_id
                || state.claim.run_context.run_id != run_id
            {
                continue;
            }
            let Some(record) = state.release.filter(|record| record.class.excludes_crew()) else {
                continue;
            };
            if record.class.suppresses_host() && released.host.is_none() {
                released.host = Some(record.clone());
            }
            released.tasks.insert(state.claim.task_id, record);
        }
        Ok(released)
    }

    /// The candidate a claim's failure or release preserved, with the spec
    /// it answered to; `None` when the leaf ended without one.
    pub(super) fn preserve_candidate(
        &self,
        task_id: &str,
        evidence: &ClaimEvidence,
    ) -> Result<Option<PreservedClaimCandidate>, OrbitError> {
        let Some(candidate) = evidence
            .failure
            .as_ref()
            .and_then(|failure| failure.candidate.clone())
        else {
            return Ok(None);
        };
        Ok(Some(PreservedClaimCandidate {
            candidate,
            task_spec_digest: self.full_task(task_id)?.spec_digest(),
            recorded_at: Utc::now().to_rfc3339(),
        }))
    }

    /// The candidate the task's latest claim settlement preserved, while it
    /// still answers to the task: no operator discarded it since, and the
    /// task's spec is unchanged [ORB-14257]. Admission hands it to the next
    /// claim's leaf to resume.
    pub(in super::super) fn resumable_candidate(
        &self,
        task: &orbit_types::task::Task,
    ) -> Result<Option<ClaimCandidateRef>, OrbitError> {
        let mut latest: Option<(
            chrono::DateTime<chrono::FixedOffset>,
            PreservedClaimCandidate,
        )> = None;
        for row in self.coordination_rows(STATE)? {
            let state: ClaimInspection = decode(&row.payload_json)?;
            if state.claim.task_id != task.id {
                continue;
            }
            let Some(preserved) = state.preserved_candidate else {
                continue;
            };
            let recorded = chrono::DateTime::parse_from_rfc3339(&preserved.recorded_at)
                .map_err(|e| OrbitError::Store(e.to_string()))?;
            if latest.as_ref().is_none_or(|(at, _)| *at < recorded) {
                latest = Some((recorded, preserved));
            }
        }
        let Some((recorded, preserved)) = latest else {
            return Ok(None);
        };
        if preserved.task_spec_digest != task.spec_digest() {
            return Ok(None);
        }
        let discarded = self
            .bundle_store
            .read_bundle_lightweight(&task.id)?
            .events
            .iter()
            .any(|event| {
                event.event_type == orbit_types::task::CANDIDATE_DISCARDED_EVENT
                    && event.at >= recorded
            });
        Ok((!discarded).then_some(preserved.candidate))
    }

    fn full_task(&self, task_id: &str) -> Result<orbit_types::task::Task, OrbitError> {
        crate::repository::task::v2::TaskV2Store::new(
            self.registry.clone(),
            self.workspace_id.clone(),
        )
        .task_from_bundle(self.bundle_store.read_bundle(task_id)?)
    }

    /// The typed failure releases of `task_id` that count against its
    /// release budget at `now`, oldest first: those inside the window and
    /// after the last release that exhausted the budget, which a human has
    /// since answered by unblocking the task.
    pub(super) fn budgeted_releases(
        &self,
        task_id: &str,
        now: chrono::DateTime<Utc>,
    ) -> Result<Vec<ClaimReleaseRecord>, OrbitError> {
        let at = |record: &ClaimReleaseRecord| {
            chrono::DateTime::parse_from_rfc3339(&record.released_at)
                .map(|at| at.with_timezone(&Utc))
                .map_err(|e| OrbitError::Store(e.to_string()))
        };
        let mut releases = Vec::new();
        for row in self.coordination_rows(STATE)? {
            let state: ClaimInspection = decode(&row.payload_json)?;
            if state.claim.task_id != task_id {
                continue;
            }
            if let Some(record) = state.release.filter(|record| record.class.budgeted()) {
                releases.push((at(&record)?, record));
            }
        }
        let window_start = now - chrono::Duration::hours(RELEASE_BUDGET_WINDOW_HOURS);
        let since = releases
            .iter()
            .filter(|(_, record)| record.budget_exhausted)
            .map(|(at, _)| *at)
            .max()
            .map_or(window_start, |exhausted| exhausted.max(window_start));
        releases.retain(|(at, record)| !record.budget_exhausted && *at > since);
        releases.sort_by_key(|(at, _)| *at);
        Ok(releases.into_iter().map(|(_, record)| record).collect())
    }

    pub(super) fn with_friction_result(
        &self,
        mut result: ClaimMutationResult,
        receipt_id: &str,
    ) -> Result<ClaimMutationResult, OrbitError> {
        if let Some(row) = self.coordination_row("distributed-claim-friction-v1", receipt_id)? {
            result.friction = Some(decode(&row.payload_json)?);
        }
        Ok(result)
    }
}

/// The releases one drain run made for failures that blame its host.
#[derive(Debug, Default)]
pub(in super::super) struct DrainReleases {
    /// Each task released for a class that excludes the leaf's crew.
    pub(in super::super) tasks: BTreeMap<String, ClaimReleaseRecord>,
    /// The first release whose class suppresses the whole host.
    pub(in super::super) host: Option<ClaimReleaseRecord>,
}

/// The one comment a task blocked by its release budget carries: every
/// release the budget counted, and the failure that exceeded it.
pub(super) fn release_budget_comment(
    earlier: &[ClaimReleaseRecord],
    failure: &ClaimFailure,
    released_at: &chrono::DateTime<Utc>,
) -> String {
    let mut comment = format!(
        "Blocked: follower drains released this claim {} times within {RELEASE_BUDGET_WINDOW_HOURS}h \
         for failures that were not the candidate's, and it failed again. A human has to \
         decide what changes before it is pulled again. Every reason:",
        earlier.len()
    );
    let now = released_at.to_rfc3339();
    let reasons = earlier
        .iter()
        .map(|record| {
            (
                record.released_at.as_str(),
                record.class,
                record.reason.as_str(),
            )
        })
        .chain(std::iter::once((
            now.as_str(),
            failure.class,
            failure.reason.as_str(),
        )));
    for (at, class, reason) in reasons {
        comment.push_str(&format!("\n- {at} {}: {reason}", class.as_str()));
    }
    comment
}
