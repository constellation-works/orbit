//! Crew executor `host_os` limits shared by assignment, draws and admission.
//!
//! An omitted declaration is unrestricted. A declared set is ordinary executor
//! data: no provider is special-cased here.

use std::collections::{BTreeMap, BTreeSet};

use orbit_common::OrbitError;
use orbit_config::executor_host_os;
use orbit_types::identity::Crew;
use orbit_types::task::{ExecutorOsLimit, HostOs, Task, TaskOsRequirement};

use crate::OrbitRuntime;
use crate::application::job::crew_pools::CapturedCrewPools;

impl OrbitRuntime {
    /// Executors that declare `host_os`, keyed by executor name.
    pub(crate) fn declared_executor_host_os(
        &self,
    ) -> Result<BTreeMap<String, BTreeSet<HostOs>>, OrbitError> {
        let mut declared = BTreeMap::new();
        for def in self.list_executor_defs()? {
            if let Some(hosts) = def.host_os {
                declared.insert(def.name, hosts);
            }
        }
        Ok(declared)
    }

    /// Crews whose executor declares `host_os`, keyed by crew name.
    pub(crate) fn executor_host_os_limits(
        &self,
    ) -> Result<BTreeMap<String, ExecutorOsLimit>, OrbitError> {
        let declared = self.declared_executor_host_os()?;
        let mut limits = BTreeMap::new();
        for (name, crew) in self.context.settings().crews() {
            if let Some(limit) = limit_for_crew(crew, &declared) {
                limits.insert(name.clone(), limit);
            }
        }
        Ok(limits)
    }

    /// Whether `crew`'s executor can run on this host.
    ///
    /// An omitted `host_os` can. A declared set cannot when this host's OS is
    /// unknown or outside the set. `declared` is the map from
    /// [`Self::declared_executor_host_os`].
    pub(crate) fn crew_runs_on_host(
        &self,
        crew: &Crew,
        declared: &BTreeMap<String, BTreeSet<HostOs>>,
    ) -> bool {
        match executor_host_os(&crew.assignment.provider, declared) {
            None => true,
            Some(hosts) => self.host_os().is_some_and(|host| hosts.contains(&host)),
        }
    }

    /// The OS wait for an assigned task, or for the default its automatic
    /// route actually reaches. Inspect the same candidates as the draw so a
    /// supported purpose/complexity pool can still run under an unsupported
    /// default. Non-OS resolution failures retain their existing handling.
    pub(crate) fn task_host_os_wait(
        &self,
        task: &Task,
        pools: &CapturedCrewPools,
        limits: &BTreeMap<String, ExecutorOsLimit>,
    ) -> Option<String> {
        match TaskOsRequirement::for_crew(&task.tags, task.crew.as_deref(), limits) {
            Ok(requirement) => {
                if let Some(wait) = requirement.unsatisfied_reason(self.host_os()) {
                    return Some(wait);
                }
            }
            Err(reason) => return Some(reason),
        }
        if limits.is_empty() {
            return None;
        }
        // Candidate resolution's only policy refusal is the default executor
        // OS check. Disabled/missing crews use InvalidInput instead, and are
        // still diagnosed by the dispatch/allowlist gates that own them.
        match self.unheld_task_crew_candidates(task, pools) {
            Err(OrbitError::PolicyDenied(wait)) => Some(wait),
            _ => None,
        }
    }

    /// Refuse a crew assignment whose executor `host_os` shares no host with
    /// the task's `os:` tags. The task is not written.
    pub(crate) fn refuse_unsatisfiable_os_crew(
        &self,
        tags: &[String],
        crew: &str,
    ) -> Result<(), OrbitError> {
        let limits = self.executor_host_os_limits()?;
        match orbit_types::task::TaskOsRequirement::for_crew(tags, Some(crew), &limits) {
            Ok(_) => Ok(()),
            Err(reason) => Err(OrbitError::InvalidInput(reason)),
        }
    }
}

fn limit_for_crew(
    crew: &Crew,
    declared: &BTreeMap<String, BTreeSet<HostOs>>,
) -> Option<ExecutorOsLimit> {
    let provider = crew.assignment.provider.trim();
    if let Some(hosts) = declared.get(provider) {
        return Some(ExecutorOsLimit {
            executor: provider.to_string(),
            hosts: hosts.clone(),
        });
    }
    let (executor, hosts) = declared
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(provider))?;
    Some(ExecutorOsLimit {
        executor: (*executor).clone(),
        hosts: hosts.clone(),
    })
}
