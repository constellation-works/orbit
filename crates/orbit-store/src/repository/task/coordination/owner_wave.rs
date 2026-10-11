//! An owner-local drain wave's admission, serialized with pull grants
//! [ORB-15514].
//!
//! The drain selects its wave from a snapshot and its gate reserves the
//! footprint only later, so a pull grant between the two could hand an
//! overlapping task to a follower, and a grant just before the wave could be
//! missed by its snapshot. Each wave therefore passes the same exclusive
//! section a pull admission takes: a candidate a live claim holds, or whose
//! footprint overlaps one, is refused, and every admitted footprint is
//! published as a coordination row the pull's screen honours until its gate
//! has had time to reserve it.

use chrono::{DateTime, Duration, Utc};
use orbit_common::OrbitError;
use orbit_types::task::TaskStatus;
use serde::{Deserialize, Serialize};

use super::TaskCommitBoundary;
use super::admission::overlaps;
use crate::contracts::*;

const OWNER_WAVE_KIND: &str = "owner-wave-admission-v1";

/// How long an admitted footprint stays visible to pull grants. It covers the
/// gap from the wave to its gate's first reservation, which is seconds; a
/// gate that has not reserved by then waits like any other, and a wave whose
/// dispatch never happened holds nothing for longer.
const OWNER_WAVE_VISIBILITY_SECONDS: i64 = 900;

#[derive(Serialize, Deserialize)]
struct OwnerWaveRow {
    task_id: String,
    footprint: Vec<String>,
    expires_at: DateTime<Utc>,
}

impl TaskCommitBoundary {
    /// Admit `candidates` for the owner's local drain inside the partition's
    /// admission section, in order. A candidate whose task a live execution
    /// claim holds, or whose footprint overlaps one, comes back with that
    /// claim; every other candidate's footprint is visible to each pull grant
    /// that enters the section after this one.
    pub fn admit_owner_wave(
        &self,
        candidates: &[OwnerWaveCandidate],
    ) -> Result<Vec<OwnerWaveAdmission>, OrbitError> {
        if candidates.is_empty() {
            return Ok(Vec::new());
        }
        self.with_admission(|| {
            let claims = self.execution_claims()?;
            let expires_at = Utc::now() + Duration::seconds(OWNER_WAVE_VISIBILITY_SECONDS);
            let mut admissions = Vec::with_capacity(candidates.len());
            for candidate in candidates {
                if let Some(claim) = claims.iter().find(|claim| {
                    claim.phase.protects_footprint()
                        && (claim.task_id == candidate.task_id
                            || overlaps(&candidate.footprint, &claim.footprint))
                }) {
                    admissions.push(OwnerWaveAdmission {
                        task_id: candidate.task_id.clone(),
                        blocked_by: Some(Box::new(claim.clone())),
                    });
                    continue;
                }
                self.publish_owner_wave_row(&OwnerWaveRow {
                    task_id: candidate.task_id.clone(),
                    footprint: candidate.footprint.clone(),
                    expires_at,
                })?;
                admissions.push(OwnerWaveAdmission::admitted(&candidate.task_id));
            }
            Ok(admissions)
        })
    }

    /// One row per task, replaced by its next admission.
    fn publish_owner_wave_row(&self, wave: &OwnerWaveRow) -> Result<(), OrbitError> {
        let payload =
            serde_json::to_string(wave).map_err(|error| OrbitError::Store(error.to_string()))?;
        match self.store.task_coordination_row(
            &self.workspace_id,
            OWNER_WAVE_KIND,
            &wave.task_id,
        )? {
            Some(previous) => {
                if !self.store.replace_task_coordination_payload(
                    &self.workspace_id,
                    &previous,
                    &payload,
                )? {
                    return Err(OrbitError::Store(
                        "owner wave admission changed inside the serialization boundary".into(),
                    ));
                }
                Ok(())
            }
            None => self.store.insert_task_coordination_row(
                &self.workspace_id,
                &TaskCoordinationRow {
                    kind: OWNER_WAVE_KIND.into(),
                    row_id: wave.task_id.clone(),
                    payload_json: payload,
                },
            ),
        }
    }

    /// The footprints the owner's local drain admitted and its gates may not
    /// have reserved yet, by task: unexpired, and only while the task is
    /// still `backlog`. Once it leaves `backlog` its in-flight footprint or
    /// its end speaks for it. Caller holds the admission section.
    pub(super) fn owner_wave_footprints_locked(
        &self,
    ) -> Result<Vec<(String, Vec<String>)>, OrbitError> {
        let now = Utc::now();
        let mut footprints = Vec::new();
        for row in self
            .store
            .task_coordination_rows(&self.workspace_id, OWNER_WAVE_KIND)?
        {
            let wave: OwnerWaveRow = serde_json::from_str(&row.payload_json)
                .map_err(|error| OrbitError::Store(error.to_string()))?;
            if wave.expires_at <= now {
                continue;
            }
            let backlog = self
                .bundle_store
                .read_bundle_if_settled(&wave.task_id)?
                .is_some_and(|bundle| bundle.envelope.status == TaskStatus::Backlog);
            if backlog {
                footprints.push((wave.task_id, wave.footprint));
            }
        }
        Ok(footprints)
    }
}
