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
    /// there. A release for a forge outage blames neither [ORB-14634].
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
            let Some(record) = state
                .release
                .filter(|record| record.class.excludes_crew() && !record.forge_unavailable)
            else {
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
    /// it answered to; `None` when the leaf ended without one. A release for
    /// an evidence hold keeps the held candidate from the branch its leaf
    /// published it to, so the run after the evidence arrives resumes it.
    pub(super) fn preserve_candidate(
        &self,
        task_id: &str,
        evidence: &ClaimEvidence,
    ) -> Result<Option<PreservedClaimCandidate>, OrbitError> {
        let held = evidence.evidence_hold.as_ref().and_then(|hold| {
            let branch = hold.published_ref.as_deref()?.strip_prefix("refs/heads/")?;
            Some(ClaimCandidateRef {
                branch: branch.to_string(),
                head_sha: hold.candidate.commit.clone(),
                pull_request: None,
                source_run_id: Some(hold.run_id.clone()),
                // Complete: its review held only for evidence, so a claim
                // that resumes it continues rather than repairs a verdict.
                failed_step_id: None,
                published: true,
                durable_ref: None,
                carry_failure: None,
            })
        });
        let Some(candidate) = evidence
            .failure
            .as_ref()
            .and_then(|failure| failure.candidate.clone())
            .or(held)
        else {
            return Ok(None);
        };
        Ok(Some(PreservedClaimCandidate {
            candidate,
            task_spec_digest: self.full_task(task_id)?.spec_digest(),
            recorded_at: Utc::now().to_rfc3339(),
        }))
    }

    /// What the owner hands a run of `task` executing on `machine_id` — a
    /// claim's leaf [ORB-14257] or its own run [ORB-14603] — from the
    /// candidate the task's latest claim settlement preserved: the candidate
    /// itself while it still answers to the task — no operator discarded it
    /// since, the task's spec is unchanged — and that host can fetch it
    /// [ORB-14338]. Otherwise the run implements afresh, with the typed
    /// reason the task's history records. `None` when no claim of the task
    /// preserved one.
    pub(in super::super) fn candidate_offer(
        &self,
        task: &orbit_types::task::Task,
        machine_id: &str,
    ) -> Result<Option<KeptClaimCandidate>, OrbitError> {
        let mut latest: Option<(
            chrono::DateTime<chrono::FixedOffset>,
            PreservedClaimCandidate,
            ExecutionClaim,
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
                latest = Some((recorded, preserved, state.claim));
            }
        }
        let Some((recorded, preserved, claim)) = latest else {
            return Ok(None);
        };
        let source_machine = claim.executed_on.machine_id;
        let candidate = preserved.candidate;
        let fresh = if !task.spec_digest_matches(&preserved.task_spec_digest) {
            Some((
                CandidateFreshReason::SpecChanged,
                "the task's description or acceptance criteria changed since it was kept".into(),
            ))
        } else if self
            .bundle_store
            .read_bundle_lightweight(&task.id)?
            .events
            .iter()
            .any(|event| {
                event.event_type == orbit_types::task::CANDIDATE_DISCARDED_EVENT
                    && event.at >= recorded
            })
        {
            Some((
                CandidateFreshReason::Discarded,
                "an operator discarded it since it was kept".into(),
            ))
        } else if !candidate.durable() && source_machine != machine_id {
            let why = match &candidate.carry_failure {
                Some(failure) => format!("pushing it to a durable ref failed ({failure})"),
                None => "its leaf ended without pushing it to a durable ref".into(),
            };
            Some((
                CandidateFreshReason::NotDurable,
                format!("it exists only on {source_machine}, which committed it: {why}"),
            ))
        } else {
            None
        };
        Ok(Some(KeptClaimCandidate {
            claim_id: claim.claim_id,
            machine_id: source_machine,
            candidate,
            fresh,
        }))
    }

    /// What admission offers a claim of `task` on `machine_id`: the
    /// candidate an owner-local run of the task held [ORB-14905] while the
    /// task is still linked to that run, else the candidate its latest claim
    /// settlement kept. Each comes with why the claim implements afresh
    /// instead, if it does.
    pub(in super::super) fn admission_offer(
        &self,
        task: &orbit_types::task::Task,
        history: &[orbit_types::task::TaskHistoryEntry],
        machine_id: &str,
    ) -> Result<Option<CandidateOffer>, OrbitError> {
        if let Some(held) = held_candidate_offer(task, history, machine_id) {
            return Ok(Some(held));
        }
        Ok(self
            .candidate_offer(task, machine_id)?
            .map(|kept| CandidateOffer {
                candidate: kept.candidate,
                fresh: kept.fresh,
            }))
    }

    /// [ORB-14603] `Self::candidate_offer` for the owner's own run of
    /// `task_id` on `machine_id`: the task's last claim failed, and its run
    /// continues the candidate that claim kept rather than implementing anew.
    pub fn kept_claim_candidate(
        &self,
        task_id: &str,
        machine_id: &str,
    ) -> Result<Option<KeptClaimCandidate>, OrbitError> {
        self.candidate_offer(&self.full_task(task_id)?, machine_id)
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
            if let Some(record) = state.release.filter(ClaimReleaseRecord::budgeted) {
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

/// A candidate admission offers a claim, and why the claim implements afresh
/// instead, if it does.
#[derive(Debug)]
pub(in super::super) struct CandidateOffer {
    pub(in super::super) candidate: ClaimCandidateRef,
    pub(in super::super) fresh: Option<(CandidateFreshReason, String)>,
}

/// [ORB-14905] The candidate the task's latest owner-local hold kept, as
/// offered to a claim on `machine_id`: a red base, a missing validation tool
/// or a failed provider held it without judging it, and its run pushed it to
/// a durable ref on `origin` unless that push failed. Offered only while the
/// task is still linked to that run, so a later run on any host supersedes
/// it, and only while the task's spec is unchanged, no operator discarded it
/// since, and `machine_id` can fetch it.
fn held_candidate_offer(
    task: &orbit_types::task::Task,
    history: &[orbit_types::task::TaskHistoryEntry],
    machine_id: &str,
) -> Option<CandidateOffer> {
    use orbit_types::workflow::{CANDIDATE_HELD_EVENT, HeldCandidate};

    let entry = history
        .iter()
        .rev()
        .find(|entry| entry.event == CANDIDATE_HELD_EVENT)?;
    let held = HeldCandidate::from_text(entry.note.as_deref()?)?;
    if task.job_run_id.as_deref() != Some(held.run_id.as_str()) {
        return None;
    }
    // Run ids are unique only per machine [ORB-13649].
    if let Some(linked) = &task.job_run_machine
        && held.machine_id.as_deref() != Some(linked.machine_id.as_str())
    {
        return None;
    }
    let holder = held
        .machine_id
        .clone()
        .unwrap_or_else(|| "the host that held it".into());
    let fresh = if !task.spec_digest_matches(&held.task_spec_digest) {
        Some((
            CandidateFreshReason::SpecChanged,
            "the task's description or acceptance criteria changed since it was held".into(),
        ))
    } else if history.iter().any(|later| {
        later.event == orbit_types::task::CANDIDATE_DISCARDED_EVENT && later.at >= entry.at
    }) {
        Some((
            CandidateFreshReason::Discarded,
            "an operator discarded it since it was held".into(),
        ))
    } else if held.durable_ref.is_none() && held.machine_id.as_deref() != Some(machine_id) {
        let why = match &held.carry_failure {
            Some(failure) => format!("pushing it to a durable ref failed ({failure})"),
            None => "its run did not push it to a durable ref".into(),
        };
        Some((
            CandidateFreshReason::NotDurable,
            format!("it exists only on {holder}, whose run held it: {why}"),
        ))
    } else {
        None
    };
    Some(CandidateOffer {
        candidate: ClaimCandidateRef {
            branch: held.branch,
            head_sha: held.head_sha,
            pull_request: None,
            source_run_id: Some(held.run_id),
            // None of these holds judged the candidate on its merits, so a
            // claim continues it rather than repairs a review verdict.
            failed_step_id: None,
            published: false,
            durable_ref: held.durable_ref,
            carry_failure: held.carry_failure,
        },
        fresh,
    })
}

/// The `candidate_resume` history entry a claim admitted on `machine_id`
/// records when it cannot resume the offered candidate, naming the claim, the
/// candidate and the typed reason [ORB-14338].
pub(in super::super) fn fresh_offer_history(
    offer: &CandidateOffer,
    claim_id: &str,
    machine_id: &str,
) -> Option<orbit_types::task::TaskHistoryEntry> {
    let (reason, detail) = offer.fresh.as_ref()?;
    let candidate = &offer.candidate;
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
