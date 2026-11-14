//! The live execution claims behind a lock wait [ORB-15514].
//!
//! Frozen claim footprints remain authoritative independently of the owner's
//! task envelopes and local leaf runs. Admission and readiness include those
//! surfaces, except for known no-diff holders, and name the claim behind each
//! lock wait. Every claim still fences admission of its own task.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::fs::lock_mode::FootprintLockModes;
use orbit_common::fs::selector::canonical_selector_in_workspace;
use orbit_store::contracts::{ExecutionClaim, claim_refusing_work_start};
use orbit_types::task::{Task, is_no_diff_expected};
use serde_json::{Value, json};

use crate::OrbitRuntime;

/// Each task a live execution claim holds, with that claim.
#[derive(Debug, Default)]
pub(crate) struct ClaimHolders {
    by_task: BTreeMap<String, ExecutionClaim>,
}

impl ClaimHolders {
    /// Diagnostic claim annotations. An unavailable journal leaves waits
    /// unannotated; admission uses the strict read below for its lock map.
    pub(crate) fn read(runtime: &OrbitRuntime) -> Self {
        match Self::read_for_admission(runtime) {
            Ok(claims) => claims,
            Err(error) => {
                tracing::warn!("lock waits do not name execution claims: {error}");
                Self::default()
            }
        }
    }

    /// Read protecting claims without treating an unavailable ledger as empty.
    pub(crate) fn read_for_admission(runtime: &OrbitRuntime) -> Result<Self, OrbitError> {
        Ok(Self {
            by_task: runtime
                .inspect_live_execution_claims()?
                .into_iter()
                .map(|inspection| (inspection.claim.task_id.clone(), inspection.claim))
                .collect(),
        })
    }

    /// The protecting claims among `claims`, already read.
    pub(crate) fn from_claims(claims: &[ExecutionClaim]) -> Self {
        Self {
            by_task: claims
                .iter()
                .filter(|claim| claim.phase.protects_footprint())
                .map(|claim| (claim.task_id.clone(), claim.clone()))
                .collect(),
        }
    }

    /// Include frozen surfaces even when no active task envelope holds them.
    /// Known no-diff tasks hold no context lock; an unknown owner still does.
    pub(crate) fn extend_lock_holders(
        &self,
        holders: &mut BTreeMap<String, Vec<String>>,
        task_lookup: &BTreeMap<String, Task>,
    ) {
        for claim in self.by_task.values() {
            if task_lookup
                .get(&claim.task_id)
                .is_some_and(|task| is_no_diff_expected(&task.tags))
            {
                continue;
            }
            for selector in &claim.footprint {
                let tasks = holders.entry(selector.clone()).or_default();
                if let Err(index) = tasks.binary_search(&claim.task_id) {
                    tasks.insert(index, claim.task_id.clone());
                }
            }
        }
    }

    /// A protecting claim still fences its own task, including no-diff work.
    pub(crate) fn protecting_task_claim(&self, task_id: &str) -> Option<&ExecutionClaim> {
        self.by_task.get(task_id)
    }

    /// The protecting claim that would refuse `task` starting work, judged
    /// over its whole canonical context exactly as the work-start guard
    /// judges it, unlocked paths included [ORB-23894]. Admission defers the
    /// task on this answer rather than create a run that guard fails.
    /// A claim `no_diff_holder` names holds no context lock for other tasks,
    /// as in [`Self::extend_lock_holders`]; it still fences its own task.
    /// With no such claim the answer is `None` before any selector is
    /// canonicalized, since canonicalizing reads the workspace on disk; the
    /// store's work-start guard returns early the same way.
    pub(crate) fn refusing_work_start(
        &self,
        task: &Task,
        workspace_root: &Path,
        lock_modes: &FootprintLockModes,
        no_diff_holder: impl Fn(&str) -> bool,
    ) -> Option<(&ExecutionClaim, Vec<String>)> {
        let mut claims = self
            .by_task
            .values()
            .filter(|claim| claim.task_id == task.id || !no_diff_holder(&claim.task_id))
            .peekable();
        claims.peek()?;
        let footprint = task
            .context_files
            .iter()
            .filter_map(|selector| canonical_selector_in_workspace(selector, workspace_root).ok())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let claim = claim_refusing_work_start(claims, &footprint, lock_modes)?;
        let overlapping = footprint
            .into_iter()
            .filter(|selector| {
                claim
                    .footprint
                    .iter()
                    .any(|held| lock_modes.selectors_block(selector, held))
            })
            .collect();
        Some((claim, overlapping))
    }

    /// The claims holding `task_ids`, as readiness reports them.
    pub(crate) fn json_for<'a>(&self, task_ids: impl IntoIterator<Item = &'a str>) -> Vec<Value> {
        self.held(task_ids).map(claim_json).collect()
    }

    /// One line naming the claims holding `task_ids`, or `None` when no live
    /// claim holds any of them.
    pub(crate) fn detail_for<'a>(
        &self,
        task_ids: impl IntoIterator<Item = &'a str>,
    ) -> Option<String> {
        let details = self.held(task_ids).map(claim_detail).collect::<Vec<_>>();
        (!details.is_empty()).then(|| details.join("; "))
    }

    fn held<'a, 'b>(
        &'a self,
        task_ids: impl IntoIterator<Item = &'b str>,
    ) -> impl Iterator<Item = &'a ExecutionClaim> {
        let mut task_ids = task_ids.into_iter().collect::<Vec<_>>();
        task_ids.sort_unstable();
        task_ids.dedup();
        task_ids
            .into_iter()
            .filter_map(|task_id| self.by_task.get(task_id))
    }
}

/// What a wait says about the claim that holds its blocker.
pub(crate) fn claim_detail(claim: &ExecutionClaim) -> String {
    let location = &claim.executed_on;
    format!(
        "{} is held by a live execution claim on {} (claim {}, run {})",
        claim.task_id,
        location
            .machine_name
            .as_deref()
            .unwrap_or(location.machine_id.as_str()),
        claim.claim_id,
        claim.run_context.run_id,
    )
}

pub(crate) fn claim_json(claim: &ExecutionClaim) -> Value {
    json!({
        "task_id": claim.task_id,
        "claim_id": claim.claim_id,
        "machine_id": claim.executed_on.machine_id,
        "machine_name": claim.executed_on.machine_name,
        "run_id": claim.run_context.run_id,
        "phase": claim.phase,
    })
}
