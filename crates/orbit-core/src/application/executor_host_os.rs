//! Crew executor `host_os` limits shared by assignment, draws and admission.
//!
//! An omitted declaration is unrestricted. A declared set is ordinary executor
//! data: no provider is special-cased here.

use std::collections::{BTreeMap, BTreeSet};

use orbit_common::OrbitError;
use orbit_config::executor_host_os;
use orbit_types::identity::Crew;
use orbit_types::task::{ExecutorOsLimit, HostOs};

use crate::OrbitRuntime;

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
