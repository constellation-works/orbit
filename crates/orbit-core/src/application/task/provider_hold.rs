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
//!
//! [ORB-16379] Local admission reads the task's hold as this machine sees
//! it, so an unavailable hold placed on another host excludes nothing here,
//! and widens it by this host's live crew outages, so one failed login keeps
//! every task off that crew here until the outage lapses. Pull admission
//! reads the hold as the executor's machine sees it and leaves the owner's
//! outages out: they are not the executor's.

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_config::ProviderLimitExplicitCrews;
use orbit_types::task::Task;
use orbit_types::workflow::{ProviderFailureClass, ProviderFailureHold};

use crate::OrbitRuntime;
use crate::application::job::crew_pools::{CapturedCrewPools, CrewCandidate};
use crate::runtime::task::provider_hold::same_provider;

impl OrbitRuntime {
    /// The hold this host's local admission draws `task` under: its standing
    /// hold as this machine sees it, widened by this host's crew outages. A
    /// hold that excludes no crew already defers the task, so it is not
    /// widened. Outages alone make an unavailable hold naming the latest
    /// outage's run. An unreadable outage store is logged and adds nothing.
    pub(crate) fn admission_provider_hold(&self, task: &Task) -> Option<ProviderFailureHold> {
        let hold = self.provider_hold_on(task, self.automation_machine_identity());
        if hold
            .as_ref()
            .is_some_and(|hold| hold.excluded_crews.is_empty())
        {
            return hold;
        }
        let outages = self
            .provider_crew_outages(Utc::now())
            .unwrap_or_else(|error| {
                tracing::warn!("could not read this host's provider outages: {error}");
                Vec::new()
            });
        // A crew of the same name configured on another provider is not the
        // one whose login failed.
        let registry = self.context.settings().crews();
        let mut outages = outages.into_iter().filter(|outage| {
            outage.provider.is_none()
                || registry.get(&outage.crew).is_none_or(|crew| {
                    same_provider(Some(&crew.assignment.provider), outage.provider.as_deref())
                })
        });
        let Some(latest) = outages.next() else {
            return hold;
        };
        let mut hold = hold.unwrap_or_else(|| ProviderFailureHold {
            class: ProviderFailureClass::Unavailable,
            provider: latest.provider.clone(),
            excluded_crews: Vec::new(),
            machine: None,
            carried_crews: Vec::new(),
            not_before: latest.until,
            run_id: latest.run_id.clone(),
        });
        for outage in std::iter::once(latest).chain(outages) {
            hold.excluded_crews.push(outage.crew);
            hold.not_before = hold.not_before.max(outage.until);
        }
        hold.excluded_crews.sort();
        hold.excluded_crews.dedup();
        Some(hold)
    }

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
