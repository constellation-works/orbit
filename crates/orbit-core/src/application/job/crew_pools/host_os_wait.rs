//! The host-OS wait a task meets on this host, judged over the same crew
//! candidates as the draw. `task` reads it through
//! `TaskJobPort::host_os_wait_for_task`; the executor `host_os` limits it
//! takes are task-owned (`task/executor_host_os.rs`).

use std::collections::BTreeMap;

use orbit_common::OrbitError;
use orbit_types::task::{ExecutorOsLimit, Task, TaskOsRequirement};

use super::CapturedCrewPools;
use crate::OrbitRuntime;

impl OrbitRuntime {
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
        self.task_host_os_wait_with_admitted_crew(task, pools, limits, None)
    }

    /// A frozen run selection replaces the automatic fallback, while the
    /// task's stored crew and OS tags remain constraints on discovery.
    pub(crate) fn task_host_os_wait_with_admitted_crew(
        &self,
        task: &Task,
        pools: &CapturedCrewPools,
        limits: &BTreeMap<String, ExecutorOsLimit>,
        admitted_crew: Option<&str>,
    ) -> Option<String> {
        match TaskOsRequirement::for_crew(&task.tags, task.crew.as_deref(), limits) {
            Ok(requirement) => {
                if let Some(wait) = requirement.unsatisfied_reason(self.host_os()) {
                    return Some(wait);
                }
            }
            Err(reason) => return Some(reason),
        }
        if let Some(crew) = admitted_crew {
            return match TaskOsRequirement::for_crew(&task.tags, Some(crew), limits) {
                Ok(requirement) => requirement.unsatisfied_reason(self.host_os()),
                Err(reason) => Some(reason),
            };
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
}
