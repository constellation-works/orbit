//! Crew executor `host_os` limits shared by assignment, draws and admission.
//!
//! An omitted declaration is unrestricted. A declared set is ordinary executor
//! data: no provider is special-cased here. The wait a task meets over the
//! draw's crew candidates is `job`'s (`job/crew_pools/host_os_wait.rs`),
//! which task code reads through `TaskJobPort::host_os_wait_for_task`.

use std::collections::{BTreeMap, BTreeSet};

use orbit_common::OrbitError;
use orbit_config::executor_host_os;
use orbit_types::identity::Crew;
use orbit_types::task::{ExecutorOsLimit, HostOs, Task, TaskOsRequirement};

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

    /// Whether `crew`'s executor can run on any host serving with this one:
    /// this host, or a registered host this workspace's recent claim
    /// records show pulling here (see `serving_host_oses`). Creation-time
    /// draws use this wider set so an owner can assign a crew only a
    /// follower can run; the task then waits for that follower under the
    /// ordinary effective-OS intersection. Admission keeps
    /// [`Self::crew_runs_on_host`].
    pub(crate) fn crew_runs_on_serving_host(
        &self,
        crew: &Crew,
        declared: &BTreeMap<String, BTreeSet<HostOs>>,
    ) -> bool {
        self.crew_runs_on_hosts(crew, declared, &self.serving_host_oses())
    }

    /// Whether `crew`'s executor can run on any OS in `hosts`, for a caller
    /// that resolved the serving set once for several candidates.
    pub(crate) fn crew_runs_on_hosts(
        &self,
        crew: &Crew,
        declared: &BTreeMap<String, BTreeSet<HostOs>>,
        hosts: &BTreeSet<HostOs>,
    ) -> bool {
        match executor_host_os(&crew.assignment.provider, declared) {
            None => true,
            Some(allowed) => hosts.iter().any(|host| allowed.contains(host)),
        }
    }

    /// Which registered hosts run an OS `task`'s `os:` tags and assigned
    /// crew admit, and whether each is pulling here, for a read surface that
    /// names a host-OS wait (see `host_os_fleet_note`). `None` when that
    /// requirement names no OS, as for a wait only the default crew's
    /// executor imposes, or when it cannot be resolved.
    pub fn task_host_fleet_note(&self, task: &Task) -> Option<String> {
        let limits = self.executor_host_os_limits().ok()?;
        let requirement =
            TaskOsRequirement::for_crew(&task.tags, task.crew.as_deref(), &limits).ok()?;
        if !requirement.invalid.is_empty() {
            return None;
        }
        self.host_os_fleet_note(&requirement.any_of)
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
