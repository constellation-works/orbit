//! Admission under a provider failure hold [ORB-14266].
//!
//! Run finalization holds a task whose run its provider failed
//! (`runtime::task::provider_hold`). While the hold stands, the task's crew is
//! drawn from the crews it does not exclude: its own crew or pool first, then
//! its complexity pool, then the workspace default. When the hold excludes
//! every one, the local drain's backlog snapshot defers the task until the
//! hold's `not_before`.
//!
//! [ORB-14697] A usage-limit hold on a task whose crew is its explicit choice
//! follows `workflow.provider_limit_explicit_crews`: under `wait` it does not
//! redirect the task, which waits out the hold instead.

use orbit_common::OrbitError;
use orbit_config::ProviderLimitExplicitCrews;
use orbit_types::task::Task;
use orbit_types::workflow::{ProviderFailureClass, ProviderFailureHold};

use crate::OrbitRuntime;
use crate::application::job::crew_pools::{CapturedCrewPools, CrewCandidate};

impl OrbitRuntime {
    /// The crews `task` may be admitted to under a standing hold, or `None`
    /// when the hold excludes every crew it could run as. A hold that excludes
    /// no crew at all leaves nothing to narrow, so it is `None` as well.
    /// `candidates` and `source` are the draw without the hold; the fallbacks
    /// are the task's complexity pool, then the workspace default.
    pub(crate) fn provider_held_candidates(
        &self,
        task: &Task,
        pools: &CapturedCrewPools,
        hold: &ProviderFailureHold,
        candidates: &[CrewCandidate],
        source: &str,
    ) -> Result<Option<(Vec<CrewCandidate>, String)>, OrbitError> {
        if hold.excluded_crews.is_empty() {
            return Ok(None);
        }
        let permitted = |candidates: &[CrewCandidate]| -> Vec<CrewCandidate> {
            candidates
                .iter()
                .filter(|candidate| !hold.excludes(&candidate.crew.name))
                .cloned()
                .collect()
        };
        let drawable = |candidates: &[CrewCandidate]| candidates.iter().any(|c| c.weight > 0);
        let held = |source: &str| {
            format!(
                "{source}; provider hold excludes {} until {}",
                hold.excluded_crews.join(", "),
                hold.not_before.to_rfc3339()
            )
        };
        let own = permitted(candidates);
        if drawable(&own) {
            return Ok(Some((own, held(source))));
        }
        if hold.class == ProviderFailureClass::Limit
            && self.context.settings().provider_limit().explicit_crews
                == ProviderLimitExplicitCrews::Wait
            && self.explicit_task_crew(task)?
        {
            return Ok(None);
        }
        if let Some((pool, pool_source)) =
            self.complexity_pool_candidates(task.complexity, pools)?
        {
            let pool = permitted(&pool);
            if drawable(&pool) {
                return Ok(Some((pool, held(&pool_source))));
            }
        }
        if let Ok(default) = self.resolve_crew_for_task(None, None)
            && !hold.excludes(&default.name)
        {
            return Ok(Some((
                vec![CrewCandidate {
                    crew: default,
                    weight: 1,
                }],
                held("default"),
            )));
        }
        Ok(None)
    }

    /// Why `task` waits out a provider failure hold at this admission:
    /// `Some` when the hold excludes every crew it could run as, `None` when
    /// no hold stands or another crew may take it.
    pub(crate) fn provider_backoff_deferral(
        &self,
        task: &Task,
        pools: &CapturedCrewPools,
    ) -> Result<Option<String>, OrbitError> {
        let Some(hold) = self.admission_provider_hold(task) else {
            return Ok(None);
        };
        // A hold that excludes no crew has no draw to narrow, so it defers
        // whatever the task would otherwise draw, even when that draw fails.
        if !hold.excluded_crews.is_empty() {
            let (candidates, source) = self.unheld_task_crew_candidates(task, pools)?;
            if self
                .provider_held_candidates(task, pools, &hold, &candidates, &source)?
                .is_some()
            {
                return Ok(None);
            }
        }
        let excluded = if hold.excluded_crews.is_empty() {
            "the run resolved no crew to exclude".to_string()
        } else {
            format!(
                "every crew the task may run as ({}) is excluded",
                hold.excluded_crews.join(", ")
            )
        };
        Ok(Some(format!(
            "run {} ended with {}; {excluded} until {}",
            hold.run_id,
            hold.class.as_str(),
            hold.not_before.to_rfc3339()
        )))
    }

    /// The crews `task` is drawn from before this host's provider limits:
    /// its unheld draw, narrowed by a standing provider hold when the hold
    /// leaves any crew, and that hold.
    pub(crate) fn held_crew_candidates(
        &self,
        task: &Task,
        pools: &CapturedCrewPools,
    ) -> Result<(Vec<CrewCandidate>, String, Option<ProviderFailureHold>), OrbitError> {
        let (candidates, source) = self.unheld_task_crew_candidates(task, pools)?;
        let Some(hold) = self.admission_provider_hold(task) else {
            return Ok((candidates, source, None));
        };
        let (candidates, source) = self
            .provider_held_candidates(task, pools, &hold, &candidates, &source)?
            .unwrap_or((candidates, source));
        Ok((candidates, source, Some(hold)))
    }
}
