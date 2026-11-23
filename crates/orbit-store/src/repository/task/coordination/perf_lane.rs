//! Same-host holders of the perf lane, read under the admission section.
//!
//! The paired-protocol lock is one flock per host, so a pull serialises perf
//! tasks on the requester's machine only. The section is the authority: two
//! pulls can both snapshot an empty lane, and the second must see the claim
//! the first committed.

use std::collections::{BTreeMap, BTreeSet};

use orbit_common::OrbitError;
use orbit_types::task::{
    PERF_LANE_TAGS, Task, TaskEnvelopeV2, TaskStatus, needs_perf_lane, validate_orb_task_id,
};

use super::TaskCommitBoundary;
use crate::contracts::{ExecutionClaim, TaskIndexFilter};
use crate::repository::task::v2_bundle::TaskBundleStoreV2;

impl TaskCommitBoundary {
    /// Read `task_id`'s settled envelope from whichever workspace partition on
    /// this host holds it.
    fn read_envelope_across_partitions(
        &self,
        task_id: &str,
    ) -> Result<Option<TaskEnvelopeV2>, OrbitError> {
        let foreign_partition = match self.registry.find_task_binding(task_id)? {
            Some(binding) if binding.partition_id != self.workspace_id => {
                Some(binding.partition_id)
            }
            _ => None,
        };
        match foreign_partition {
            Some(partition_id) => TaskBundleStoreV2::new(self.registry.clone(), partition_id)
                .read_envelope_if_settled(task_id),
            None => self.bundle_store.read_envelope_if_settled(task_id),
        }
    }

    /// Whether `task_id`'s settled envelope needs the perf lane, resolving its
    /// partition through the registry so claims from another workspace on this
    /// host are checked against their own partition. An unknown id, or one
    /// still being written, returns `false`.
    pub(super) fn task_needs_perf_lane(&self, task_id: &str) -> Result<bool, OrbitError> {
        if validate_orb_task_id(task_id).is_err() {
            return Ok(false);
        }
        let envelope = match self.read_envelope_across_partitions(task_id) {
            Ok(envelope) => envelope,
            Err(OrbitError::NotFound { .. }) => None,
            Err(error) => return Err(error),
        };
        Ok(envelope.is_some_and(|task| needs_perf_lane(&task.tags)))
    }

    /// Perf tasks that already hold the lane on `machine_id` across every
    /// workspace partition on this owner host.
    ///
    /// A protecting claim on that machine in any partition always counts. When
    /// `count_owner_local` is set, so do an `in-progress` perf task with no
    /// protecting claim (in this or any other partition), an owner-wave perf
    /// task still in `backlog` (in this or any other partition), and a carrier
    /// the caller named. A `review` task holds only through a protecting claim
    /// or a carrier, so an idle review perf task releases the lane. The
    /// candidate itself may appear; the screen skips it.
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
            if task.status == TaskStatus::InProgress
                && needs_perf_lane(&task.tags)
                && !protecting.contains(task.id.as_str())
            {
                holders.insert(task.id.clone());
            }
        }
        // Scan indexed in-progress perf tasks across partitions whose
        // in-flight tasks were not preloaded above; the envelope confirms the
        // tag.
        let filter = TaskIndexFilter {
            statuses: vec![TaskStatus::InProgress],
            any_tags: PERF_LANE_TAGS
                .iter()
                .map(|tag| (*tag).to_string())
                .collect(),
            ..TaskIndexFilter::default()
        };
        let mut partitions = self.registry.partition_ids()?;
        partitions.insert(self.workspace_id.clone());
        for partition_id in partitions {
            if partition_id == self.workspace_id && !in_flight_tasks.is_empty() {
                continue;
            }
            for id in self
                .registry
                .indexed_task_ids_filtered(&partition_id, &filter)?
            {
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
        holders.extend(self.other_partition_owner_wave_perf_tasks_locked()?);
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
