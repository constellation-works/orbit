//! An owner-local drain wave's admission, serialized with pull grants
//! [ORB-15514].
//!
//! The drain selects its wave from a snapshot and its gate reserves the
//! footprint only later, so a pull grant between the two could hand an
//! overlapping task to a follower, and a grant just before the wave could be
//! missed by its snapshot. Each wave therefore passes the same exclusive
//! section a pull admission takes: a candidate a live claim holds, or whose
//! footprint overlaps an ordinary claim, is refused. Known no-diff claims
//! fence only their own task. Every admitted footprint is
//! published as a coordination row the pull's screen honours until its gate
//! has had time to reserve it. A second perf task on the owner's machine is
//! withheld instead of published, so the lane stays one task wide.

use std::collections::BTreeSet;

use chrono::{DateTime, Duration, Utc};
use orbit_common::OrbitError;
use orbit_types::task::{TaskStatus, is_no_diff_expected, validate_orb_task_id};
use serde::{Deserialize, Serialize};

use super::TaskCommitBoundary;
use super::selection::DependencyReach;
use crate::contracts::*;
use crate::repository::task::v2_bundle::TaskBundleStoreV2;

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
    /// claim holds, or whose footprint overlaps a claim without a known
    /// no-diff exemption, comes back with that claim; every other candidate's
    /// footprint is visible to each pull grant that enters the section after
    /// this one.
    ///
    /// `owner_machine_id` is the owner's machine. A perf candidate is withheld
    /// when another perf task already holds that machine's lane: a protecting
    /// claim there, an in-flight perf task with no claim, an owner-wave perf
    /// task still in `backlog`, or an earlier perf candidate in this wave.
    /// `None` skips those holds; two perf candidates in one call still
    /// serialise. A withheld candidate is not published.
    pub fn admit_owner_wave(
        &self,
        candidates: &[OwnerWaveCandidate],
        owner_machine_id: Option<&str>,
    ) -> Result<Vec<OwnerWaveAdmission>, OrbitError> {
        if candidates.is_empty() {
            return Ok(Vec::new());
        }
        if let Some(admissions) = self.with_admission(|| {
            self.admit_owner_wave_locked(candidates, owner_machine_id, DependencyReach::Partition)
        })? {
            return Ok(admissions);
        }
        self.with_host_admission(|| {
            self.admit_owner_wave_locked(candidates, owner_machine_id, DependencyReach::Host)
        })?
        .ok_or_else(|| {
            OrbitError::Store("a host-wide owner-wave admission did not complete".into())
        })
    }

    fn admit_owner_wave_locked(
        &self,
        candidates: &[OwnerWaveCandidate],
        owner_machine_id: Option<&str>,
        reach: DependencyReach,
    ) -> Result<Option<Vec<OwnerWaveAdmission>>, OrbitError> {
        let (claims, host_claims) = self.partition_and_host_protecting_claims()?;
        // Read tags under the same admission boundary as the claim check.
        // No-diff claims fence their task, but hold no context locks.
        // Missing or unsettled task metadata grants no exemption.
        let mut no_diff_task_ids = BTreeSet::new();
        for claim in claims
            .iter()
            .filter(|claim| claim.phase.protects_footprint())
        {
            if validate_orb_task_id(&claim.task_id).is_err() {
                continue;
            }
            let envelope = match self.bundle_store.read_envelope_if_settled(&claim.task_id) {
                Ok(envelope) => envelope,
                Err(OrbitError::NotFound { .. }) => None,
                Err(error) => return Err(error),
            };
            if envelope.is_some_and(|task| is_no_diff_expected(&task.tags)) {
                no_diff_task_ids.insert(claim.task_id.as_str());
            }
        }
        let lock_modes = self.store.footprint_lock_modes();
        let expires_at = Utc::now() + Duration::seconds(OWNER_WAVE_VISIBILITY_SECONDS);
        let mut lane_holders = match owner_machine_id {
            Some(machine) => self.perf_lane_holders_locked(
                machine,
                true,
                &host_claims,
                &[],
                &self.owner_wave_footprints_locked()?,
                &BTreeSet::new(),
            )?,
            None => BTreeSet::new(),
        };
        let mut admissions = Vec::with_capacity(candidates.len());
        let mut rows = Vec::with_capacity(candidates.len());
        for candidate in candidates {
            if let Some(claim) = claims.iter().find(|claim| {
                claim.phase.protects_footprint()
                    && (claim.task_id == candidate.task_id
                        || (!no_diff_task_ids.contains(claim.task_id.as_str())
                            && lock_modes.footprints_block(&candidate.footprint, &claim.footprint)))
            }) {
                admissions.push(OwnerWaveAdmission {
                    task_id: candidate.task_id.clone(),
                    blocked_by: Some(Box::new(claim.clone())),
                    perf_lane_held_by: None,
                });
                continue;
            }
            let needs_lane = self.task_needs_perf_lane(&candidate.task_id)?;
            if needs_lane
                && let Some(holder) = lane_holders
                    .iter()
                    .find(|holder| holder.as_str() != candidate.task_id)
                    .cloned()
            {
                let claim = host_claims.iter().find(|claim| {
                    claim.phase.protects_footprint()
                        && claim.task_id == holder
                        && owner_machine_id
                            .is_some_and(|machine| claim.executed_on.machine_id == machine)
                });
                admissions.push(OwnerWaveAdmission {
                    task_id: candidate.task_id.clone(),
                    blocked_by: claim.cloned().map(Box::new),
                    perf_lane_held_by: Some(holder),
                });
                continue;
            }
            if needs_lane {
                // Serialize with other partitions before publishing a perf-lane
                // admission on the owner machine.
                if owner_machine_id.is_some() && reach == DependencyReach::Partition {
                    return Ok(None);
                }
                lane_holders.insert(candidate.task_id.clone());
            }
            let wave = OwnerWaveRow {
                task_id: candidate.task_id.clone(),
                footprint: candidate.footprint.clone(),
                expires_at,
            };
            rows.push(TaskCoordinationRow {
                kind: OWNER_WAVE_KIND.into(),
                row_id: wave.task_id.clone(),
                payload_json: serde_json::to_string(&wave)
                    .map_err(|error| OrbitError::Store(error.to_string()))?,
            });
            admissions.push(OwnerWaveAdmission::admitted(&candidate.task_id));
        }
        // One row per task, replaced by its next admission, and the whole
        // wave in one transaction: a wave that fails publishes none of
        // its footprints.
        self.store
            .upsert_sql_task_coordination_rows(&self.workspace_id, &rows)?;
        Ok(Some(admissions))
    }

    /// Unexpired `backlog` perf tasks admitted by owner waves in other
    /// workspace partitions on this host. Caller holds the admission section.
    pub(super) fn other_partition_owner_wave_perf_tasks_locked(
        &self,
    ) -> Result<Vec<String>, OrbitError> {
        let now = Utc::now();
        let mut tasks = Vec::new();
        for partition_id in self.registry.partition_ids()? {
            if partition_id == self.workspace_id {
                continue;
            }
            let bundle_store = TaskBundleStoreV2::new(self.registry.clone(), partition_id.clone());
            for row in self
                .store
                .task_coordination_rows(&partition_id, OWNER_WAVE_KIND)?
            {
                let wave: OwnerWaveRow = serde_json::from_str(&row.payload_json)
                    .map_err(|error| OrbitError::Store(error.to_string()))?;
                if wave.expires_at <= now {
                    continue;
                }
                let envelope = match bundle_store.read_envelope_if_settled(&wave.task_id) {
                    Ok(envelope) => envelope,
                    Err(OrbitError::NotFound { .. }) => None,
                    Err(error) => return Err(error),
                };
                if envelope.is_some_and(|task| {
                    task.status == TaskStatus::Backlog
                        && orbit_types::task::needs_perf_lane(&task.tags)
                }) {
                    tasks.push(wave.task_id);
                }
            }
        }
        Ok(tasks)
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
