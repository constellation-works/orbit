//! Same-host holders of the perf lane, read under the admission section.
//!
//! The paired-protocol lock is one flock per host, so a pull serialises perf
//! tasks on the requester's machine only. The section is the authority: two
//! pulls can both snapshot an empty lane, and the second must see the claim
//! the first committed.

use std::collections::{BTreeMap, BTreeSet};

use orbit_common::OrbitError;
use orbit_types::task::{PERF_LANE_TAGS, Task, TaskStatus, needs_perf_lane, validate_orb_task_id};

use super::TaskCommitBoundary;
use crate::contracts::{ExecutionClaim, TaskIndexFilter};

impl TaskCommitBoundary {
    /// Whether `task_id`'s settled envelope needs the perf lane. An id this
    /// partition does not hold, or one still being written, does not.
    pub(super) fn task_needs_perf_lane(&self, task_id: &str) -> Result<bool, OrbitError> {
        if validate_orb_task_id(task_id).is_err() {
            return Ok(false);
        }
        let envelope = match self.bundle_store.read_envelope_if_settled(task_id) {
            Ok(envelope) => envelope,
            Err(OrbitError::NotFound { .. }) => None,
            Err(error) => return Err(error),
        };
        Ok(envelope.is_some_and(|task| needs_perf_lane(&task.tags)))
    }

    /// Perf tasks that already hold the lane on `machine_id`.
    ///
    /// A protecting claim on that machine always counts. When
    /// `count_owner_local` is set, so do an in-flight perf task with no
    /// protecting claim, an owner-wave task still in `backlog`, and a carrier
    /// the caller named. The candidate itself may appear; the screen skips it.
    /// Caller holds the admission section.
    pub(super) fn perf_lane_holders_locked(
        &self,
        machine_id: &str,
        count_owner_local: bool,
        claims: &[ExecutionClaim],
        in_flight_tasks: &[Task],
        owner_wave: &[(String, Vec<String>)],
        carriers: &BTreeSet<String>,
    ) -> Result<BTreeSet<String>, OrbitError> {
        let mut holders = BTreeSet::new();
        let mut protecting = BTreeSet::new();
        let known = in_flight_tasks
            .iter()
            .map(|task| (task.id.clone(), needs_perf_lane(&task.tags)))
            .collect::<BTreeMap<_, _>>();
        for claim in claims
            .iter()
            .filter(|claim| claim.phase.protects_footprint())
        {
            protecting.insert(claim.task_id.as_str());
            if claim.executed_on.machine_id == machine_id
                && self.cached_needs_perf_lane(&claim.task_id, &known)?
            {
                holders.insert(claim.task_id.clone());
            }
        }
        if !count_owner_local {
            return Ok(holders);
        }
        for task in in_flight_tasks {
            if needs_perf_lane(&task.tags) && !protecting.contains(task.id.as_str()) {
                holders.insert(task.id.clone());
            }
        }
        // The owner wave has not loaded in-flight tasks. The index names them
        // under this section; the envelope confirms the tag.
        if in_flight_tasks.is_empty() {
            let ids = self.registry.indexed_task_ids_filtered(
                &self.workspace_id,
                &TaskIndexFilter {
                    statuses: vec![TaskStatus::InProgress, TaskStatus::Review],
                    any_tags: PERF_LANE_TAGS
                        .iter()
                        .map(|tag| (*tag).to_string())
                        .collect(),
                    ..TaskIndexFilter::default()
                },
            )?;
            for id in ids {
                if !protecting.contains(id.as_str()) && self.cached_needs_perf_lane(&id, &known)? {
                    holders.insert(id);
                }
            }
        }
        for (task_id, _) in owner_wave {
            if self.cached_needs_perf_lane(task_id, &known)? {
                holders.insert(task_id.clone());
            }
        }
        for task_id in carriers {
            if self.cached_needs_perf_lane(task_id, &known)? {
                holders.insert(task_id.clone());
            }
        }
        Ok(holders)
    }

    fn cached_needs_perf_lane(
        &self,
        task_id: &str,
        known: &BTreeMap<String, bool>,
    ) -> Result<bool, OrbitError> {
        if let Some(needs) = known.get(task_id) {
            return Ok(*needs);
        }
        self.task_needs_perf_lane(task_id)
    }
}
