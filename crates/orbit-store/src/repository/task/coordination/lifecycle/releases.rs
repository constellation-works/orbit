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

    /// What admission hands a claim of `task` executing on `machine_id` from
    /// the candidate the task's latest claim settlement preserved
    /// [ORB-14257]: the candidate itself while it still answers to the task —
    /// no operator discarded it since, the task's spec is unchanged — and
    /// that host can fetch it [ORB-14338]. Otherwise the claim implements
    /// afresh, with the typed reason the task's history records. `None` when
    /// no claim of the task preserved one.
    pub(in super::super) fn candidate_offer(
        &self,
        task: &orbit_types::task::Task,
        machine_id: &str,
    ) -> Result<Option<CandidateOffer>, OrbitError> {
        let mut latest: Option<(
            chrono::DateTime<chrono::FixedOffset>,
            PreservedClaimCandidate,
            String,
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
            if latest.as_ref().is_none_or(|(at, _, _)| *at < recorded) {
                latest = Some((recorded, preserved, state.claim.executed_on.machine_id));
            }
        }
        let Some((recorded, preserved, source_machine)) = latest else {
            return Ok(None);
        };
        let candidate = preserved.candidate;
        let fresh = |reason, detail: String| {
            Ok(Some(CandidateOffer::Fresh {
                candidate: candidate.clone(),
                reason,
                detail,
            }))
        };
        if !task.spec_digest_matches(&preserved.task_spec_digest) {
            return fresh(
                CandidateFreshReason::SpecChanged,
                "the task's description or acceptance criteria changed since it was kept".into(),
            );
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
        if discarded {
            return fresh(
                CandidateFreshReason::Discarded,
                "an operator discarded it since it was kept".into(),
            );
        }
        if !candidate.durable() && source_machine != machine_id {
            let why = match &candidate.carry_failure {
                Some(failure) => format!("pushing it to a durable ref failed ({failure})"),
                None => "its leaf ended without pushing it to a durable ref".into(),
            };
            return fresh(
                CandidateFreshReason::NotDurable,
                format!("it exists only on {source_machine}, which committed it: {why}"),
            );
        }
        Ok(Some(CandidateOffer::Resume(candidate)))
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

/// What admission hands a claim from the task's kept candidate.
pub(in super::super) enum CandidateOffer {
    /// The candidate, for the claim's leaf to resume.
    Resume(ClaimCandidateRef),
    /// No candidate: the claim's leaf implements afresh, and the task's
    /// history records why [ORB-14338].
    Fresh {
        candidate: ClaimCandidateRef,
        reason: CandidateFreshReason,
        detail: String,
    },
}

impl CandidateOffer {
    /// The candidate the claim's leaf resumes, if any.
    pub(in super::super) fn resume_candidate(&self) -> Option<ClaimCandidateRef> {
        match self {
            Self::Resume(candidate) => Some(candidate.clone()),
            Self::Fresh { .. } => None,
        }
    }

    /// The `candidate_resume` history entry a fresh offer records on the
    /// task, naming the claim, the candidate and the typed reason.
    pub(in super::super) fn history(
        &self,
        claim_id: &str,
        machine_id: &str,
    ) -> Option<orbit_types::task::TaskHistoryEntry> {
        let Self::Fresh {
            candidate,
            reason,
            detail,
        } = self
        else {
            return None;
        };
        Some(orbit_types::task::TaskHistoryEntry {
            at: Utc::now(),
            by: machine_id.to_string(),
            event: orbit_types::task::CANDIDATE_RESUME_EVENT.into(),
            note: Some(format!(
                "fresh: claim={claim_id}, machine={machine_id}, source_run={}, source_branch={}, \
                 source_sha={}; reason={}: candidate {} {detail}",
                candidate.source_run_id.as_deref().unwrap_or("unknown"),
                candidate.branch,
                candidate.head_sha,
                reason.as_str(),
                candidate.head_sha,
            )),
            from_status: None,
            to_status: None,
        })
    }
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
