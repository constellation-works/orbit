//! Crew executor `host_os` limits: which hosts a crew's executor declares.
//!
//! An omitted declaration is unrestricted. A declared set is ordinary executor
//! data: no provider is special-cased here.

use std::collections::{BTreeMap, BTreeSet};

use orbit_common::OrbitError;
use orbit_types::identity::Crew;
use orbit_types::task::{ExecutorOsLimit, HostOs};

use crate::RuntimeKernel;

impl RuntimeKernel {
    /// Executors that declare `host_os`, keyed by executor name.
    pub fn declared_executor_host_os(
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
    ///
    /// Enumerates the same last-known-good registry crew selection resolves
    /// from (`admission_crew_settings`), not the startup settings, so a crew
    /// added by a live config edit is limited exactly as in a fresh runtime.
    pub fn executor_host_os_limits(&self) -> Result<BTreeMap<String, ExecutorOsLimit>, OrbitError> {
        let declared = self.declared_executor_host_os()?;
        let mut limits = BTreeMap::new();
        let crew_settings = self.admission_crew_settings();
        for (name, crew) in &crew_settings.crews {
            if let Some(limit) = limit_for_crew(crew, &declared) {
                limits.insert(name.clone(), limit);
            }
        }
        Ok(limits)
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
